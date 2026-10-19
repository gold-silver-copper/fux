//! `bench --instructions`: each engine's work per workload in instructions
//! retired, which, unlike MB/s, does not change with the machine's load.
//!
//! Each engine and workload runs in a child process of its own (this
//! binary again, [`CHILD`]), counted by `count::Counter` as `fux-bench
//! --against` counts (`/usr/bin/time -l` on macOS). The child makes the
//! workload as `bench` does, makes the engine as `bench` does, under each
//! retention (`bench::Retention`: equal budget, 10,000 rows asked; equal
//! retention, no history), and feeds it in 4 KiB chunks; it prints the rows
//! its engine held, read back. Its baseline is the same child without the
//! feeding: making the bytes and the engine. An engine's count is the
//! median of its paired rounds (each run less its round's baseline), per
//! byte fed; its noise, the widest round's deviation from it (`side`). The engine
//! is never dropped, in either, so freeing its memory is not counted.
//!
//! Only the engines in this process are counted: an engine behind a
//! process of its own does its work in that process, which the count does
//! not see.
use crate::bench::{self, CHUNK, COLS, Load, ROWS};
use crate::count::Counter;
use crate::engine::{ENGINES, Kind, SUBJECT, Setup};
use std::fmt::Write as _;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

/// The hidden subcommand a child runs: `CHILD ENGINE WORKLOAD MB RETENTION
/// [baseline]`.
pub const CHILD: &str = "--instructions-child";

/// The named workload, `mb` MiB of it, alone, as `bench` makes it.
fn load(name: &str, mb: usize) -> Result<Load, String> {
    bench::load(name, mb.saturating_mul(1 << 20))
}

fn kind(name: &str) -> Result<&'static Kind, String> {
    std::iter::once(&SUBJECT)
        .chain(ENGINES)
        .find(|k| k.name == name && k.in_process)
        .ok_or(format!("no engine in this process named {name:?}"))
}

/// The child: makes the workload and the engine set up for RETENTION
/// (`bench::Retention`), and feeds it unless this is the baseline. Prints
/// the bytes made and the rows of history the engine holds after (`?` if
/// it cannot tell), read outside anything counted that matters: the
/// baseline reads them too.
pub fn child(args: &[String]) -> Result<(), String> {
    let (engine, workload, mb, retention, baseline) = match args {
        [engine, workload, mb, retention] => (engine, workload, mb, retention, false),
        [engine, workload, mb, retention, b] if b == "baseline" => {
            (engine, workload, mb, retention, true)
        }
        _ => return Err(format!("{CHILD} ENGINE WORKLOAD MB RETENTION [baseline]")),
    };
    let mb: usize = mb.parse().map_err(|e| format!("MB: {e}"))?;
    let retention =
        bench::Retention::parse(retention).ok_or(format!("no retention {retention:?}"))?;
    let load = load(workload, mb)?;
    let kind = kind(engine)?;
    let setup = Setup {
        rows: load.rows,
        cols: load.cols,
        history: 10_000,
        reflow: true,
    };
    // Kept to the end of the process: dropping it would count freeing.
    let made = retention
        .make(kind, &setup)
        .ok_or(format!("{engine}: no {} setup", retention.name()))??;
    let vt = Box::leak(made);
    if !baseline {
        load.feed(vt)?;
    }
    let rows = vt
        .history_len()
        .map_or_else(|| "?".to_owned(), |r| r.to_string());
    println!("{} {rows}", load.bytes.len());
    Ok(())
}

/// One child to count.
struct Task {
    /// The column: an engine with a retention.
    engine: usize,
    workload: usize,
    baseline: bool,
}

/// An engine's count of one workload, by paired rounds, as fux-bench
/// counts (speed-method.md): each round's run less that round's baseline,
/// the count their median, and the spread the widest deviation of a
/// round from it, over the count. A round at or below its baseline counts
/// nothing: then there is no count (`side` says none).
#[derive(Clone, Copy, Debug, PartialEq)]
struct Side {
    count: i64,
    noise: f64,
}

fn side(full: &[i64], baseline: &[i64]) -> Option<Side> {
    let mut rounds: Vec<i64> = full
        .iter()
        .zip(baseline)
        .map(|(f, b)| f.saturating_sub(*b))
        .collect();
    if rounds.is_empty() || full.len() != baseline.len() || rounds.iter().any(|r| *r <= 0) {
        return None;
    }
    rounds.sort_unstable();
    let mid = rounds.len() / 2;
    let count = match rounds.len() % 2 {
        1 => *rounds.get(mid)?,
        _ => {
            rounds
                .get(mid.checked_sub(1)?)?
                .saturating_add(*rounds.get(mid)?)
                / 2
        }
    };
    let widest = rounds.iter().map(|r| r.abs_diff(count)).max()?;
    (count > 0).then(|| Side {
        count,
        noise: (widest as f64) / (count as f64),
    })
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
    let (counter, counter_setup) = Counter::detect()?;
    let kinds: Vec<&Kind> = std::iter::once(&SUBJECT)
        .chain(engines.iter().filter_map(|&i| ENGINES.get(i)))
        .collect();
    let skipped: Vec<&str> = kinds
        .iter()
        .filter(|k| !k.in_process)
        .map(|k| k.name)
        .collect();
    // Each engine in process, under each retention it can be set up for.
    let probe = Setup {
        rows: ROWS,
        cols: COLS,
        history: 10_000,
        reflow: true,
    };
    let columns: Vec<(&Kind, bench::Retention)> = bench::Retention::ALL
        .into_iter()
        .flat_map(|r| {
            kinds
                .iter()
                .filter(|k| k.in_process)
                .filter(move |k| r.make(k, &probe).is_some())
                .map(move |k| (*k, r))
        })
        .collect();
    let workloads = bench::chosen(names)?;
    // Each workload made once here, to check its name, give its size and
    // its hash.
    let loads: Vec<(usize, String, String)> = workloads
        .iter()
        .map(|w| {
            let l = load(w, mb)?;
            let hash = bench::sha256(&l.bytes)?;
            Ok((l.bytes.len(), l.about, hash))
        })
        .collect::<Result<_, String>>()?;
    let mut tasks = Vec::new();
    for _ in 0..repeats.max(1) {
        for workload in 0..workloads.len() {
            for engine in 0..columns.len() {
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
        "{} children ({} engines and retentions, {} workloads, {} repeats), {jobs} at a time, \
         counted by {}",
        tasks.len(),
        columns.len(),
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
                    let (engine, retention) = columns
                        .get(task.engine)
                        .map_or(("", ""), |(k, r)| (k.name, r.name()));
                    let workload = workloads.get(task.workload).map_or("", String::as_str);
                    let mut args = vec![CHILD, engine, workload, mb_text.as_str(), retention];
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
    let mut results = results
        .into_inner()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    // In the tasks' order, so each round's run and baseline pair up.
    results.sort_by_key(|(i, _)| *i);

    // Each engine's and workload's counts, runs and baselines.
    let mut counts = vec![vec![[Vec::new(), Vec::new()]; workloads.len()]; columns.len()];
    // The rows each column's engine held after a feed, read back.
    let mut held = vec![vec![None; workloads.len()]; columns.len()];
    let mut failed = Vec::new();
    for (i, result) in &results {
        let Some(task) = tasks.get(*i) else {
            continue;
        };
        let fed = loads.get(task.workload).map(|(n, _, _)| n.to_string());
        let result = result.as_ref().map_err(Clone::clone).and_then(|run| {
            // The child prints the bytes it made, the same as here, and the
            // rows its engine held.
            let mut words = run.stdout.split_whitespace();
            if words.next() == fed.as_deref() {
                Ok((run, words.next().map(str::to_owned)))
            } else {
                Err(format!("the child made {:?} bytes", run.stdout.trim()))
            }
        });
        match result {
            Ok((run, rows)) => {
                if !task.baseline
                    && let Some(slot) = held
                        .get_mut(task.engine)
                        .and_then(|h| h.get_mut(task.workload))
                {
                    *slot = rows;
                }
                if let Some(list) = counts
                    .get_mut(task.engine)
                    .and_then(|c| c.get_mut(task.workload))
                    .and_then(|c| c.get_mut(usize::from(task.baseline)))
                {
                    list.push(i64::try_from(run.instructions).unwrap_or(i64::MAX));
                }
            }
            Err(e) => failed.push(format!(
                "{} {}: {e}",
                columns.get(task.engine).map_or("?", |(k, _)| k.name),
                workloads.get(task.workload).map_or("?", String::as_str)
            )),
        }
    }
    for f in &failed {
        eprintln!("bench --instructions: {f}");
    }

    let cpus = bench::cpu_set();
    let mut text = String::new();
    let mut rows = Vec::new();
    let mut worst = 0f64;
    for retention in bench::Retention::ALL {
        let here: Vec<(usize, &Kind)> = columns
            .iter()
            .enumerate()
            .filter(|(_, (_, r))| *r == retention)
            .map(|(i, (k, _))| (i, *k))
            .collect();
        if here.is_empty() {
            continue;
        }
        let _ = write!(
            text,
            "\n{} ({}), in process: instructions per byte, the median of {} paired \
             rounds, each run less its round's baseline (making the bytes and the engine), no \
             read counted; \
             {mb} MiB per workload, {CHUNK}-byte chunks (NAME@N: N), {ROWS}x{COLS} (recordings at their \
             size); fux-vt reflow on, print recording off; counted by {} on CPUs {cpus} \
             (the counter's own set is in the JSON)\n{:<34}",
            retention.name(),
            retention.about(),
            repeats.max(1),
            counter.name(),
            ""
        );
        for (_, kind) in &here {
            let _ = write!(text, " {:>10}", kind.name);
        }
        let _ = writeln!(text);
        for (w, workload) in workloads.iter().enumerate() {
            let _ = write!(text, "{workload:<34}");
            let (bytes, about, hash) = loads
                .get(w)
                .map_or((0, "", ""), |(n, a, h)| (*n, a.as_str(), h.as_str()));
            let mut per_engine = serde_json::Map::new();
            let mut held_line = String::new();
            for (c, kind) in &here {
                let counted = counts.get(*c).and_then(|v| v.get(w));
                let cell = counted.and_then(|[full, base]| side(full, base));
                let rows_held = held.get(*c).and_then(|h| h.get(w)).cloned().flatten();
                let _ = write!(held_line, " {:>10}", rows_held.as_deref().unwrap_or("-"));
                // Equal retention holds the screen alone: an engine that
                // read back rows kept is not counted on it.
                // Rows it can't tell are not held, as `bench` takes them.
                let kept = retention == bench::Retention::None
                    && (rows_held.as_deref())
                        .and_then(|r| r.parse::<usize>().ok())
                        .is_some_and(|r| r > 0);
                match cell {
                    Some(s) if !kept => {
                        worst = worst.max(s.noise);
                        let per_byte = (s.count as f64) / (bytes.max(1) as f64);
                        let _ = write!(text, " {per_byte:>10.2}");
                        per_engine.insert(
                            kind.name.to_owned(),
                            serde_json::json!({
                                "instructions": s.count,
                                "per_byte": per_byte,
                                "noise_percent": s.noise * 100.0,
                                "rows_held": rows_held,
                                // Every round's counts, run and baseline,
                                // in order: the figures are made from them.
                                "runs": counted.map(|[full, _]| full),
                                "baselines": counted.map(|[_, base]| base),
                            }),
                        );
                    }
                    _ => {
                        let why = if kept {
                            format!(
                                "held {} rows under equal retention",
                                rows_held.as_deref().unwrap_or("?")
                            )
                        } else if counted.is_some_and(|[f, b]| !f.is_empty() && !b.is_empty()) {
                            "counted at or below its baseline".to_owned()
                        } else {
                            "no count".to_owned()
                        };
                        let why = format!("{} {workload} ({}): {why}", kind.name, retention.name());
                        eprintln!("bench --instructions: {why}");
                        failed.push(why);
                        let _ = write!(text, " {:>10}", "error");
                    }
                }
            }
            let _ = writeln!(text, "\n{:<34}{held_line}  rows held", "");
            rows.push(serde_json::json!({
                "name": workload,
                "retention": retention.name(),
                "about": about,
                "bytes": bytes,
                "sha256": hash,
                "chunk_bytes": bench::chunk_of(workload),
                "engines": per_engine,
            }));
        }
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
            "counter_setup": counter_setup,
            "repeats": repeats.max(1),
            "mb": mb,
            "chunk": CHUNK,
            "engines": columns.iter().map(|(k, r)| format!("{} {}", k.name, r.name())).collect::<Vec<_>>(),
            "fairness": {
                "chunk_bytes": CHUNK,
                "read_counted": false,
                "reflow": "fux-vt on; the others as they are",
                "print_recording": "off",
                "in_process": true,
                "cpus": cpus,
                "repeats": repeats.max(1),
                "retentions": bench::Retention::ALL.map(|r| serde_json::json!({"name": r.name(), "setup": r.about()})),
                "kitty_images": "ignored-protocol throughput: fux-vt keeps no kitty images",
                "sixel": "ignored-protocol throughput for an engine without sixel",
            },
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
    fn a_count_is_the_median_of_paired_rounds() {
        // Rounds of 1002, 1000, 999: the median 1000, the widest 2 off.
        let side = super::side(&[1103, 1100, 1101], &[101, 100, 102]);
        assert!(side.is_some_and(|s| s.count == 1000 && (s.noise - 2.0 / 1000.0).abs() < 1e-9));
        // A round at or below its baseline, or none: no count.
        assert_eq!(super::side(&[100, 104], &[100, 101]), None);
        assert_eq!(super::side(&[90], &[100]), None);
        assert_eq!(super::side(&[], &[100]), None);
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
