//! `fux-walk`: seeded random walks over a real fux, with every invariant
//! checked after every step, traces that replay the steps taken, and
//! failing walks minimized to the steps that matter.
mod fixture;
mod generate;
mod notices;
mod rng;
mod run;
mod step;
mod world;

use run::{Checker, End, Failure};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use step::Step;

const VERSION: &str = "fux-walk trace 1";

const USAGE: &str = "\
usage: fux-walk --fux PATH [--seed N] [--steps N] [--seeds N] [--seconds N]
                [--settle MS] [--minimize-seconds N] [--output DIR]
       fux-walk --fux PATH --replay FILE [--settle MS]
       fux-walk --fux PATH --replay-all DIR [--settle MS]";

struct Options {
    fux: PathBuf,
    seed: u64,
    steps: usize,
    seeds: u64,
    seconds: u64,
    settle: Duration,
    minimize: Duration,
    output: PathBuf,
    replay: Option<PathBuf>,
    replay_all: Option<PathBuf>,
    stop: Arc<AtomicBool>,
}

fn options() -> Result<Options, String> {
    let mut args = std::env::args().skip(1);
    let mut fux = None;
    let mut o = Options {
        fux: PathBuf::new(),
        seed: 1,
        steps: 300,
        seeds: 1,
        seconds: 3600,
        settle: Duration::from_secs(5),
        minimize: Duration::from_secs(900),
        output: PathBuf::from("walk/runs"),
        replay: None,
        replay_all: None,
        stop: Arc::new(AtomicBool::new(false)),
    };
    let number = |value: Option<String>, flag: &str| -> Result<u64, String> {
        value
            .ok_or(format!("{flag} needs a value"))?
            .parse()
            .map_err(|e| format!("{flag}: {e}"))
    };
    while let Some(flag) = args.next() {
        match flag.as_str() {
            "--fux" => fux = args.next(),
            "--seed" => o.seed = number(args.next(), &flag)?,
            "--steps" => {
                o.steps = usize::try_from(number(args.next(), &flag)?).map_err(|e| e.to_string())?
            }
            "--seeds" => o.seeds = number(args.next(), &flag)?,
            "--seconds" => o.seconds = number(args.next(), &flag)?,
            "--settle" => o.settle = Duration::from_millis(number(args.next(), &flag)?),
            "--minimize-seconds" => o.minimize = Duration::from_secs(number(args.next(), &flag)?),
            "--output" => o.output = PathBuf::from(args.next().ok_or("--output needs a value")?),
            "--replay" => o.replay = args.next().map(PathBuf::from),
            "--replay-all" => o.replay_all = args.next().map(PathBuf::from),
            "--help" | "-h" => return Err(USAGE.into()),
            other => return Err(format!("unknown argument {other:?}\n{USAGE}")),
        }
    }
    // Never built here, never looked up on PATH: the binary named, made
    // absolute before anything changes directory.
    let fux = fux.ok_or(format!("--fux is required\n{USAGE}"))?;
    o.fux = Path::new(&fux)
        .canonicalize()
        .map_err(|e| format!("--fux {fux}: {e}"))?;
    Ok(o)
}

/// A trace file's steps.
fn read_trace(path: &Path) -> Result<Vec<Step>, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    text.lines()
        .filter(|l| !l.trim().is_empty() && !l.starts_with('#') && *l != VERSION)
        .map(Step::parse)
        .collect()
}

fn write_trace(path: &Path, steps: &[Step], note: &str) -> Result<(), String> {
    let mut text = format!("{VERSION}\n");
    for line in note.lines() {
        text.push_str(&format!("# {line}\n"));
    }
    for step in steps {
        text.push_str(&step.line());
        text.push('\n');
    }
    std::fs::write(path, text).map_err(|e| format!("{}: {e}", path.display()))
}

/// FNV-1a over the binary: enough to tell two builds apart.
fn identity(path: &Path) -> String {
    let bytes = std::fs::read(path).unwrap_or_default();
    let hash = bytes.iter().fold(0xcbf2_9ce4_8422_2325u64, |h, b| {
        (h ^ u64::from(*b)).wrapping_mul(0x0100_0000_01b3)
    });
    format!("{} bytes, fnv1a {hash:016x}", bytes.len())
}

fn metadata(o: &Options, seed: u64) -> String {
    let uname = std::process::Command::new("uname")
        .arg("-a")
        .output()
        .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_owned())
        .unwrap_or_default();
    let version = std::process::Command::new(&o.fux)
        .arg("--version")
        .output()
        .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_owned())
        .unwrap_or_default();
    format!(
        "seed {seed}\nsteps {}\nfux {}\nbinary {}\nversion {version}\nsystem {uname}\nsettle {:?}\n",
        o.steps,
        o.fux.display(),
        identity(&o.fux),
        o.settle
    )
}

/// Takes `steps` on a fresh server: where it stopped short, if it did.
fn replay(o: &Options, steps: &[Step]) -> Result<(), End> {
    let mut fixture = fixture::Fixture::start(&o.fux).map_err(|e| {
        End::Failed(Failure {
            at: 0,
            invariant: "the fixture starts".into(),
            detail: e,
        })
    })?;
    let mut checker = Checker::new(o.settle);
    let result = (|| {
        for (at, step) in steps.iter().enumerate() {
            if o.stop.load(Ordering::SeqCst) {
                break;
            }
            let before = run::overlays(&mut fixture);
            run::take(&mut fixture, step, at)?;
            checker.check(&mut fixture, step, at, &before)?;
        }
        Ok(())
    })();
    continue_stopped(&fixture);
    let left = fixture.finish(result.is_err());
    result?;
    if !left.is_empty() {
        return Err(End::Failed(Failure {
            at: steps.len(),
            invariant: "cleanup leaves nothing running".into(),
            detail: left.join("\n"),
        }));
    }
    Ok(())
}

/// Every pane process is continued before cleanup, so that none is left
/// stopped for the server to kill.
fn continue_stopped(fixture: &fixture::Fixture) {
    if let Ok(world) = world::World::read(fixture) {
        for pane in world.panes() {
            if let Some(pid) = fuxix::process::Pid::from_raw(pane.pid) {
                let _ = fuxix::process::kill(pid, fuxix::process::Signal::Cont);
            }
        }
    }
}

/// Whether `steps` fail with the same invariant.
fn fails_the_same(o: &Options, steps: &[Step], invariant: &str) -> bool {
    matches!(replay(o, steps), Err(End::Failed(f)) if f.invariant == invariant)
}

/// The fewest steps that still fail with `failure`'s invariant: the
/// shortest failing prefix by bisection, then steps dropped in shrinking
/// chunks, each candidate on a fresh server, within `o.minimize`.
fn minimize(o: &Options, steps: &[Step], failure: &Failure) -> Vec<Step> {
    let deadline = fixture::after(o.minimize);
    let invariant = failure.invariant.as_str();
    let mut hi = failure.at.saturating_add(1).min(steps.len());
    let mut lo = 1usize;
    while lo < hi && Instant::now() < deadline {
        let mid = lo.saturating_add(hi.saturating_sub(lo) / 2);
        if fails_the_same(o, steps.get(..mid).unwrap_or_default(), invariant) {
            hi = mid;
        } else {
            lo = mid.saturating_add(1);
        }
    }
    let mut current: Vec<Step> = steps.get(..hi).unwrap_or_default().to_vec();
    let mut chunks = 2usize;
    while current.len() >= 2 && Instant::now() < deadline {
        let size = current.len().div_ceil(chunks).max(1);
        let mut reduced = false;
        for start in (0..current.len()).filter(|i| i.checked_rem(size) == Some(0)) {
            if Instant::now() > deadline {
                break;
            }
            let candidate: Vec<Step> = current
                .iter()
                .enumerate()
                .filter(|(i, _)| *i < start || *i >= start.saturating_add(size))
                .map(|(_, s)| s.clone())
                .collect();
            if !candidate.is_empty() && fails_the_same(o, &candidate, invariant) {
                current = candidate;
                chunks = chunks.saturating_sub(1).max(2);
                reduced = true;
                break;
            }
        }
        if !reduced {
            if chunks >= current.len() {
                break;
            }
            chunks = chunks.saturating_mul(2).min(current.len());
        }
    }
    current
}

/// What one walk came to.
enum Outcome {
    Passed(usize),
    ServerDone(usize),
    Failed(Failure),
}

/// One seeded walk: its steps are generated as it goes and saved as a
/// trace, and a failure is saved and minimized.
fn walk(o: &Options, seed: u64, deadline: Instant) -> Result<Outcome, String> {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let dir = o.output.join(format!("{stamp}-seed-{seed}"));
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    std::fs::write(dir.join("metadata.txt"), metadata(o, seed)).map_err(|e| e.to_string())?;
    let mut fixture = fixture::Fixture::start(&o.fux)?;
    let mut checker = Checker::new(o.settle);
    let mut state = generate::State::new();
    let mut rng = rng::Rng::new(seed);
    let mut trace: Vec<Step> = Vec::new();
    let result = (|| {
        for at in 0..o.steps {
            if o.stop.load(Ordering::SeqCst) || Instant::now() > deadline {
                break;
            }
            let world = world::World::read(&fixture).map_err(|e| {
                End::Failed(Failure {
                    at,
                    invariant: "the server answers".into(),
                    detail: e,
                })
            })?;
            let clients: Vec<String> = fixture
                .clients
                .keys()
                .filter(|id| world.clients.iter().any(|c| &c.id == *id))
                .cloned()
                .collect();
            let step = generate::step(&mut rng, &world, &clients, &state);
            trace.push(step.clone());
            let before = run::overlays(&mut fixture);
            run::take(&mut fixture, &step, at)?;
            checker.check(&mut fixture, &step, at, &before)?;
            state.after(&step);
        }
        Ok(())
    })();
    continue_stopped(&fixture);
    let failed = matches!(result, Err(End::Failed(_)));
    let left = fixture.finish(failed);
    let kept = fixture.dir.clone();
    write_trace(&dir.join("trace.txt"), &trace, &format!("seed {seed}"))?;
    let outcome = match result {
        Ok(()) if left.is_empty() => Outcome::Passed(trace.len()),
        Ok(()) => Outcome::Failed(Failure {
            at: trace.len(),
            invariant: "cleanup leaves nothing running".into(),
            detail: left.join("\n"),
        }),
        Err(End::ServerDone) => Outcome::ServerDone(trace.len()),
        Err(End::Failed(f)) => Outcome::Failed(f),
    };
    if let Outcome::Failed(f) = &outcome {
        let server_log = std::fs::read_to_string(kept.join("server.log")).unwrap_or_default();
        std::fs::write(
            dir.join("failure.txt"),
            format!(
                "step {}: {}\ninvariant: {}\n\n{}\n\n--- server log (end) ---\n{}\n",
                f.at,
                trace.get(f.at).map(Step::line).unwrap_or_default(),
                f.invariant,
                f.detail,
                run::tail(&server_log)
            ),
        )
        .map_err(|e| e.to_string())?;
        eprintln!(
            "seed {seed}: failed at step {}: {}; minimizing",
            f.at, f.invariant
        );
        let minimized = minimize(o, &trace, f);
        let index = std::fs::read_dir(&dir)
            .map(|d| {
                d.filter(|e| {
                    e.as_ref()
                        .is_ok_and(|e| e.file_name().to_string_lossy().starts_with("minimized-"))
                })
                .count()
            })
            .unwrap_or(0)
            .saturating_add(1);
        write_trace(
            &dir.join(format!("minimized-{index:03}.txt")),
            &minimized,
            &format!(
                "seed {seed}, minimized from {} steps\ninvariant: {}",
                f.at.saturating_add(1),
                f.invariant
            ),
        )?;
        eprintln!("seed {seed}: minimized to {} steps", minimized.len());
    }
    println!("seed {seed}: run saved in {}", dir.display());
    Ok(outcome)
}

fn main() -> ExitCode {
    let o = match options() {
        Ok(o) => o,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::from(2);
        }
    };
    for signal in [signal_hook::consts::SIGINT, signal_hook::consts::SIGTERM] {
        if let Err(e) = signal_hook::flag::register(signal, Arc::clone(&o.stop)) {
            eprintln!("fux-walk: {e}");
            return ExitCode::from(2);
        }
    }
    let replays: Vec<PathBuf> = match (&o.replay, &o.replay_all) {
        (Some(file), _) => vec![file.clone()],
        (None, Some(dir)) => {
            let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
                .map(|d| {
                    d.filter_map(|e| e.ok().map(|e| e.path()))
                        .filter(|p| p.extension().is_some_and(|x| x == "txt"))
                        .collect()
                })
                .unwrap_or_default();
            files.sort();
            files
        }
        (None, None) => Vec::new(),
    };
    let mut failed = false;
    if o.replay.is_some() || o.replay_all.is_some() {
        for file in &replays {
            let start = Instant::now();
            let verdict = read_trace(file).map(|steps| (steps.len(), replay(&o, &steps)));
            match verdict {
                Ok((n, Ok(()))) => println!(
                    "{}: {n} steps, passed in {:.1?}",
                    file.display(),
                    start.elapsed()
                ),
                Ok((n, Err(End::ServerDone))) => println!(
                    "{}: {n} steps, ended as the last pane closed",
                    file.display()
                ),
                Ok((_, Err(End::Failed(f)))) => {
                    failed = true;
                    println!(
                        "{}: FAILED at step {}: {}\n{}",
                        file.display(),
                        f.at,
                        f.invariant,
                        f.detail
                    );
                }
                Err(e) => {
                    failed = true;
                    println!("{}: {e}", file.display());
                }
            }
        }
        if replays.is_empty() {
            eprintln!("no traces to replay");
            failed = true;
        }
    } else {
        let deadline = fixture::after(Duration::from_secs(o.seconds));
        for seed in o.seed..o.seed.saturating_add(o.seeds) {
            if o.stop.load(Ordering::SeqCst) || Instant::now() > deadline {
                break;
            }
            let start = Instant::now();
            match walk(&o, seed, deadline) {
                Ok(Outcome::Passed(n)) => {
                    println!("seed {seed}: {n} steps, passed in {:.1?}", start.elapsed())
                }
                Ok(Outcome::ServerDone(n)) => println!(
                    "seed {seed}: {n} steps, ended as the last pane closed, in {:.1?}",
                    start.elapsed()
                ),
                Ok(Outcome::Failed(f)) => {
                    failed = true;
                    println!(
                        "seed {seed}: FAILED at step {}: {}\n{}",
                        f.at, f.invariant, f.detail
                    );
                }
                Err(e) => {
                    failed = true;
                    println!("seed {seed}: {e}");
                }
            }
        }
    }
    if o.stop.load(Ordering::SeqCst) {
        eprintln!("fux-walk: interrupted");
        failed = true;
    }
    if failed {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}
