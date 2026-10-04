//! `bench --instructions`: each engine's work per workload in instructions
//! retired, which, unlike MB/s, does not change with the machine's load.
//!
//! Each engine and workload runs in a child process of its own (this
//! binary again, [`CHILD`]), counted by `count::Counter` as `fux-bench
//! --against` counts (`/usr/bin/time -l` on macOS). The child makes the
//! workload as `bench` does, makes the engine as `bench` does (50x200 with
//! 10,000 rows of history; the corpus at its first recording's size), and
//! feeds it in 4 KiB chunks. Its baseline is the same child without the
//! feeding: making the bytes and the engine. An engine's count is the
//! fewest instructions of its runs less the fewest of its baseline's, per
//! byte fed; its noise, the spread of its runs over that count. The engine
//! is never dropped, in either, so freeing its memory is not counted.
//!
//! Only the engines in this process are counted: an engine behind a
//! process of its own does its work in that process, which the count does
//! not see.
use crate::bench::{self, CHUNK, COLS, Load, ROWS, WORKLOADS};
use crate::count::Counter;
use crate::engine::{ENGINES, Kind, SUBJECT, Setup};
use crate::rng::Rng;
use std::fmt::Write as _;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

/// The hidden subcommand a child runs: `CHILD ENGINE WORKLOAD MB [baseline]`.
pub const CHILD: &str = "--instructions-child";

/// The named workload, `mb` MiB of it, alone: a synthetic one, `corpus`
/// (every recording in turn) or `corpus:NAME`, as `bench` makes them.
fn load(name: &str, mb: usize) -> Result<Load, String> {
    let total = mb.saturating_mul(1 << 20);
    if let Some((_, about, make)) = WORKLOADS.iter().find(|(n, _, _)| *n == name) {
        return Ok(Load {
            name: name.to_owned(),
            about: (*about).to_owned(),
            rows: ROWS,
            cols: COLS,
            bytes: make(&mut Rng::new(1), total),
        });
    }
    let recordings = crate::corpus::recordings(&[])?;
    if name == "corpus" {
        let first = recordings.first().ok_or("the corpus is empty")?;
        let all: Vec<u8> = recordings.iter().flat_map(|r| r.bytes()).collect();
        return Ok(Load {
            name: name.to_owned(),
            about: format!(
                "every recording in turn, {} bytes recorded, at {}x{}",
                all.len(),
                first.rows,
                first.cols
            ),
            rows: first.rows,
            cols: first.cols,
            bytes: bench::repeated(&all, total),
        });
    }
    let one = name
        .strip_prefix("corpus:")
        .and_then(|n| recordings.iter().find(|r| r.name == n))
        .ok_or(format!("no workload {name:?}"))?;
    let bytes = one.bytes();
    Ok(Load {
        name: name.to_owned(),
        about: format!("{}, {} bytes recorded", one.version, bytes.len()),
        rows: one.rows,
        cols: one.cols,
        bytes: bench::repeated(&bytes, total),
    })
}

fn kind(name: &str) -> Result<&'static Kind, String> {
    std::iter::once(&SUBJECT)
        .chain(ENGINES)
        .find(|k| k.name == name && k.in_process)
        .ok_or(format!("no engine in this process named {name:?}"))
}

/// The child: makes the workload and the engine, and feeds it unless this
/// is the baseline. Prints the bytes fed.
pub fn child(args: &[String]) -> Result<(), String> {
    let (engine, workload, mb, baseline) = match args {
        [engine, workload, mb] => (engine, workload, mb, false),
        [engine, workload, mb, b] if b == "baseline" => (engine, workload, mb, true),
        _ => return Err(format!("{CHILD} ENGINE WORKLOAD MB [baseline]")),
    };
    let mb: usize = mb.parse().map_err(|e| format!("MB: {e}"))?;
    let load = load(workload, mb)?;
    let kind = kind(engine)?;
    let setup = Setup {
        rows: load.rows,
        cols: load.cols,
        history: 10_000,
        reflow: true,
    };
    // Kept to the end of the process: dropping it would count freeing.
    let vt = Box::leak((kind.make)(&setup)?);
    if !baseline {
        vt.feed(&load.bytes, CHUNK)?;
    }
    println!("{}", load.bytes.len());
    Ok(())
}

/// One child to count.
struct Task {
    engine: usize,
    workload: usize,
    baseline: bool,
}

/// An engine's count of one workload: the fewest instructions of its
/// runs less the fewest of its baseline's, and the spread of its runs
/// over that count.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Side {
    count: i64,
    noise: f64,
}

fn side(full: &[i64], baseline: &[i64]) -> Side {
    let low = |v: &[i64]| v.iter().copied().min().unwrap_or(0);
    let high = full.iter().copied().max().unwrap_or(0);
    let count = low(full).saturating_sub(low(baseline));
    Side {
        count,
        noise: (high.saturating_sub(low(full)) as f64) / (count.max(1) as f64),
    }
}

/// Counts fux-vt and the in-process engines among `engines` on the named
/// workloads (default: the synthetic ones and the corpus all together),
/// `repeats` times each, `jobs` children at a time; prints instructions
/// per byte as a table, and writes it as JSON to `json` if given.
pub fn run(
    engines: &[usize],
    names: &[String],
    mb: usize,
    repeats: usize,
    jobs: usize,
    json: Option<&str>,
) -> Result<bool, String> {
    let started = Instant::now();
    let counter = Counter::detect()?;
    let kinds: Vec<&Kind> = std::iter::once(&SUBJECT)
        .chain(engines.iter().filter_map(|&i| ENGINES.get(i)))
        .collect();
    let skipped: Vec<&str> = kinds
        .iter()
        .filter(|k| !k.in_process)
        .map(|k| k.name)
        .collect();
    let kinds: Vec<&Kind> = kinds.into_iter().filter(|k| k.in_process).collect();
    let workloads: Vec<String> = if names.is_empty() {
        WORKLOADS
            .iter()
            .map(|(n, _, _)| (*n).to_owned())
            .chain(std::iter::once("corpus".to_owned()))
            .collect()
    } else {
        names.to_vec()
    };
    // Each workload made once here, to check its name and give its size.
    let loads: Vec<(usize, String)> = workloads
        .iter()
        .map(|w| load(w, mb).map(|l| (l.bytes.len(), l.about)))
        .collect::<Result<_, _>>()?;
    let mut tasks = Vec::new();
    for _ in 0..repeats.max(1) {
        for workload in 0..workloads.len() {
            for engine in 0..kinds.len() {
                for baseline in [false, true] {
                    tasks.push(Task {
                        engine,
                        workload,
                        baseline,
                    });
                }
            }
        }
    }
    eprintln!(
        "{} children ({} engines, {} workloads, {} repeats), {jobs} at a time, counted by {}",
        tasks.len(),
        kinds.len(),
        workloads.len(),
        repeats.max(1),
        counter.name()
    );
    let me = std::env::current_exe().map_err(|e| e.to_string())?;
    let mb_text = mb.to_string();
    let next = AtomicUsize::new(0);
    let done = AtomicUsize::new(0);
    let results = Mutex::new(Vec::new());
    std::thread::scope(|scope| {
        for _ in 0..jobs.max(1) {
            scope.spawn(|| {
                loop {
                    let i = next.fetch_add(1, Ordering::SeqCst);
                    let Some(task) = tasks.get(i) else {
                        break;
                    };
                    let engine = kinds.get(task.engine).map_or("", |k| k.name);
                    let workload = workloads.get(task.workload).map_or("", String::as_str);
                    let mut args = vec![CHILD, engine, workload, mb_text.as_str()];
                    if task.baseline {
                        args.push("baseline");
                    }
                    let result = counter.count(&me, &args);
                    results
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .push((i, result));
                    let finished = done.fetch_add(1, Ordering::SeqCst).saturating_add(1);
                    if finished.is_multiple_of(50) || finished == tasks.len() {
                        eprint!("\r  {finished}/{} runs", tasks.len());
                    }
                }
            });
        }
    });
    eprintln!();
    let results = results
        .into_inner()
        .unwrap_or_else(std::sync::PoisonError::into_inner);

    // Each engine's and workload's counts, runs and baselines.
    let mut counts = vec![vec![[Vec::new(), Vec::new()]; workloads.len()]; kinds.len()];
    let mut failed = Vec::new();
    for (i, result) in &results {
        let Some(task) = tasks.get(*i) else {
            continue;
        };
        match result {
            Ok((n, _)) => {
                if let Some(list) = counts
                    .get_mut(task.engine)
                    .and_then(|c| c.get_mut(task.workload))
                    .and_then(|c| c.get_mut(usize::from(task.baseline)))
                {
                    list.push(i64::try_from(*n).unwrap_or(i64::MAX));
                }
            }
            Err(e) => failed.push(format!(
                "{} {}: {e}",
                kinds.get(task.engine).map_or("?", |k| k.name),
                workloads.get(task.workload).map_or("?", String::as_str)
            )),
        }
    }
    for f in &failed {
        eprintln!("bench --instructions: {f}");
    }

    let mut text = format!(
        "instructions per byte, the fewest of {} runs less the fewest of their baseline \
         (making the bytes and the engine), {mb} MiB per workload, {CHUNK}-byte chunks, \
         {ROWS}x{COLS} (the corpus at its own size); counted by {}\n{:<16}",
        repeats.max(1),
        counter.name(),
        ""
    );
    for kind in &kinds {
        let _ = write!(text, " {:>10}", kind.name);
    }
    let _ = writeln!(text);
    let mut rows = Vec::new();
    let mut worst = 0f64;
    for (w, workload) in workloads.iter().enumerate() {
        let _ = write!(text, "{workload:<16}");
        let bytes = loads.get(w).map_or(0, |(n, _)| *n);
        let mut per_engine = serde_json::Map::new();
        for (e, kind) in kinds.iter().enumerate() {
            let cell = counts
                .get(e)
                .and_then(|c| c.get(w))
                .filter(|[full, base]| !full.is_empty() && !base.is_empty())
                .map(|[full, base]| side(full, base));
            match cell {
                Some(s) => {
                    worst = worst.max(s.noise);
                    let per_byte = (s.count as f64) / (bytes.max(1) as f64);
                    let _ = write!(text, " {per_byte:>10.2}");
                    per_engine.insert(
                        kind.name.to_owned(),
                        serde_json::json!({
                            "instructions": s.count,
                            "per_byte": per_byte,
                            "noise_percent": s.noise * 100.0,
                        }),
                    );
                }
                None => {
                    let _ = write!(text, " {:>10}", "error");
                }
            }
        }
        let _ = writeln!(text);
        rows.push(serde_json::json!({
            "name": workload,
            "about": loads.get(w).map_or("", |(_, a)| a.as_str()),
            "bytes": bytes,
            "engines": per_engine,
        }));
    }
    let seconds = started.elapsed().as_secs_f64();
    let _ = writeln!(
        text,
        "\nthe largest run-to-run spread: {:.2}%; {} children in {seconds:.0} s",
        worst * 100.0,
        tasks.len()
    );
    if !skipped.is_empty() {
        let _ = writeln!(
            text,
            "not counted, as they work in a process of their own: {}",
            skipped.join(", ")
        );
    }
    print!("{text}");
    if let Some(path) = json {
        let doc = serde_json::json!({
            "kind": "fux-vt-compare bench --instructions",
            "counter": counter.name(),
            "repeats": repeats.max(1),
            "mb": mb,
            "chunk": CHUNK,
            "engines": kinds.iter().map(|k| k.name).collect::<Vec<_>>(),
            "worst_noise_percent": worst * 100.0,
            "seconds": seconds,
            "failed": failed,
            "workloads": rows,
        });
        let text = serde_json::to_string_pretty(&doc).map_err(|e| e.to_string())?;
        std::fs::write(path, text).map_err(|e| format!("{path}: {e}"))?;
        eprintln!("wrote {path}");
    }
    Ok(failed.is_empty())
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_count_is_its_fewest_less_its_baselines_fewest() {
        let side = super::side(&[1103, 1100, 1101], &[101, 100, 102]);
        assert_eq!(side.count, 1000);
        assert!((side.noise - 3.0 / 1000.0).abs() < 1e-9);
    }

    /// A workload made alone is the one `bench` makes among all of them.
    #[test]
    fn a_workload_alone_is_benchs() -> Result<(), String> {
        let ascii = super::load("ascii", 1)?;
        let make = crate::bench::WORKLOADS
            .iter()
            .find(|(n, _, _)| *n == "ascii")
            .map(|(_, _, make)| *make)
            .ok_or("no ascii")?;
        assert_eq!(ascii.bytes, make(&mut crate::rng::Rng::new(1), 1 << 20));
        assert!(super::load("corpus", 1)?.bytes.len() >= 1 << 20);
        assert!(super::load("nothing", 1).is_err());
        Ok(())
    }
}
