//! `--against REF`: every workload on REF and on the working tree, in
//! instructions retired.
//!
//! REF is checked out in a temporary worktree, and this crate's source as
//! it is in the working tree is copied into it and built there, with fux's
//! release profile: so both sides run the same workloads, each linked
//! against its own fux, fux-vt and fuxix, as separate binaries (one binary
//! cannot link two copies of a crate of one version). The working tree's
//! side is this binary.
//!
//! Each workload runs as a child process of its own, counted (`count`),
//! and so does its baseline (`workloads`), `--repeats` times on each
//! side, the sides alternating. A side's count is the fewest instructions
//! of its runs less the fewest of its baseline's (see `Side`); its noise,
//! the spread of its runs over that count. A workload is flagged when the
//! working tree retires more than `--threshold` percent more instructions
//! than REF, and more than either side's noise.
use crate::count::Counter;
use crate::workloads;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

pub struct Options {
    pub reference: String,
    pub repeats: usize,
    pub jobs: usize,
    pub threshold: f64,
    pub only: Vec<String>,
    pub json: PathBuf,
}

/// The checkout this binary was built from.
pub fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..")
}

fn git(root: &Path, args: &[&str]) -> Result<String, String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .map_err(|e| format!("git: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
}

/// A directory for a run, and REF's worktree in it, removed when dropped.
struct Scratch {
    root: PathBuf,
    dir: PathBuf,
    worktree: Option<PathBuf>,
}

impl Drop for Scratch {
    fn drop(&mut self) {
        if let Some(worktree) = &self.worktree {
            let path = worktree.to_string_lossy();
            let _ = git(&self.root, &["worktree", "remove", "--force", &path]);
        }
        let _ = std::fs::remove_dir_all(&self.dir);
        let _ = git(&self.root, &["worktree", "prune"]);
    }
}

fn copy_tree(from: &Path, to: &Path) -> Result<(), String> {
    std::fs::create_dir_all(to).map_err(|e| format!("{}: {e}", to.display()))?;
    for entry in std::fs::read_dir(from).map_err(|e| format!("{}: {e}", from.display()))? {
        let entry = entry.map_err(|e| format!("{}: {e}", from.display()))?;
        let path = entry.path();
        let target = to.join(entry.file_name());
        if path.is_dir() {
            copy_tree(&path, &target)?;
        } else {
            std::fs::copy(&path, &target).map_err(|e| format!("{}: {e}", path.display()))?;
        }
    }
    Ok(())
}

/// Builds this crate's source, as it is in `root`, against REF's crates in
/// `worktree`: the binary.
fn build(root: &Path, worktree: &Path, target: &Path) -> Result<PathBuf, String> {
    let bench = worktree.join("bench");
    let _ = std::fs::remove_dir_all(&bench);
    copy_tree(&root.join("bench/src"), &bench.join("src"))?;
    for file in ["Cargo.toml", "Cargo.lock"] {
        let from = root.join("bench").join(file);
        if from.exists() {
            std::fs::copy(&from, bench.join(file))
                .map_err(|e| format!("{}: {e}", from.display()))?;
        }
    }
    // From the worktree, so that REF's toolchain file applies to it.
    let out = Command::new("cargo")
        .args(["build", "--release", "--quiet", "--manifest-path"])
        .arg(bench.join("Cargo.toml"))
        .arg("--target-dir")
        .arg(target)
        .current_dir(&bench)
        .output()
        .map_err(|e| format!("cargo: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "building REF failed:\n{}",
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    Ok(target.join("release/fux-bench"))
}

/// A run's count and what it printed, or why it failed.
type Counted = Result<(u64, String), String>;

/// One child run to count.
struct Task {
    workload: usize,
    side: usize,
    baseline: bool,
}

/// Counts every task on `jobs` threads; each task's count and output.
fn count_all(
    counter: Counter,
    binaries: &[PathBuf; 2],
    names: &[String],
    corpus: &Path,
    tasks: &[Task],
    jobs: usize,
) -> Vec<Counted> {
    let next = AtomicUsize::new(0);
    let corpus = corpus.to_string_lossy();
    let done = AtomicUsize::new(0);
    let mut results: Vec<(usize, Counted)> = std::thread::scope(|scope| {
        let workers: Vec<_> = (0..jobs.max(1))
            .map(|_| {
                scope.spawn(|| {
                    let mut out = Vec::new();
                    loop {
                        let i = next.fetch_add(1, Ordering::SeqCst);
                        let Some(task) = tasks.get(i) else {
                            break;
                        };
                        let name = names.get(task.workload).map_or("", String::as_str);
                        let mut args = vec!["run", name, "--corpus", &corpus];
                        if task.baseline {
                            args.push("--baseline");
                        }
                        let result = binaries
                            .get(task.side)
                            .ok_or_else(|| "no such side".to_owned())
                            .and_then(|binary| counter.count(binary, &args));
                        out.push((i, result));
                        let finished = done.fetch_add(1, Ordering::SeqCst).saturating_add(1);
                        if finished.is_multiple_of(50) || finished == tasks.len() {
                            eprint!("\r  {finished}/{} runs", tasks.len());
                        }
                    }
                    out
                })
            })
            .collect();
        workers
            .into_iter()
            .flat_map(|w| w.join().unwrap_or_default())
            .collect()
    });
    eprintln!();
    results.sort_by_key(|(i, _)| *i);
    results.into_iter().map(|(_, r)| r).collect()
}

/// A side's count of one workload: the fewest instructions any of its
/// runs retired less the fewest any run of its baseline did, and the
/// spread of its runs over that count. On macOS the count includes the
/// kernel's work for the process (interrupts, preemption, page faults),
/// which only adds: the fewest is the least disturbed.
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

/// The paint statistics a run prints: frames, and bytes painted.
fn painted(stdout: &str) -> Option<(u64, u64)> {
    let mut words = stdout.split_whitespace();
    let frames = words.nth(2)?.parse().ok()?;
    let bytes = words.next()?.parse().ok()?;
    Some((frames, bytes))
}

pub fn run(options: &Options) -> Result<bool, String> {
    if cfg!(debug_assertions) {
        return Err("a debug build: run `cargo run --release --manifest-path \
                    bench/Cargo.toml -- --against REF`, built as fux is"
            .into());
    }
    let started = Instant::now();
    let root = root().canonicalize().map_err(|e| e.to_string())?;
    let reference = git(
        &root,
        &[
            "rev-parse",
            "--verify",
            &format!("{}^{{commit}}", options.reference),
        ],
    )?;
    let head = git(&root, &["rev-parse", "HEAD"])?;
    let dirty = !git(&root, &["status", "--porcelain"])?.is_empty();
    let counter = Counter::detect()?;
    let corpus = crate::corpus::dir(&root);
    let recordings = crate::corpus::recordings(&corpus)?;
    let specs: Vec<workloads::Spec> = workloads::list(&recordings)
        .into_iter()
        .filter(|s| options.only.is_empty() || options.only.iter().any(|o| s.name.contains(o)))
        .collect();
    if specs.is_empty() {
        return Err("no workload matches".into());
    }
    let names: Vec<String> = specs.iter().map(|s| s.name.clone()).collect();

    // REF's worktree, removed after; its build is kept beside it, so that
    // only the crates under test build again next time.
    let place = root.join("bench/target/against");
    let mut scratch = Scratch {
        root: root.clone(),
        dir: place.join("run"),
        worktree: None,
    };
    let worktree = scratch.dir.join("ref");
    if worktree.exists() {
        // Left by a run that was stopped.
        let _ = git(
            &root,
            &["worktree", "remove", "--force", &worktree.to_string_lossy()],
        );
    }
    let _ = std::fs::remove_dir_all(&scratch.dir);
    let _ = git(&root, &["worktree", "prune"]);
    std::fs::create_dir_all(&scratch.dir).map_err(|e| e.to_string())?;
    // The working tree's side: this binary, copied, so a build meanwhile
    // cannot change it.
    let new = scratch.dir.join("fux-bench-new");
    let me = std::env::current_exe().map_err(|e| e.to_string())?;
    std::fs::copy(&me, &new).map_err(|e| format!("{}: {e}", me.display()))?;
    let short = reference.get(..12).unwrap_or(&reference).to_owned();
    eprintln!(
        "building {} ({short}) in a worktree, with this crate's source…",
        options.reference
    );
    git(
        &root,
        &[
            "worktree",
            "add",
            "--detach",
            "--quiet",
            &worktree.to_string_lossy(),
            &reference,
        ],
    )?;
    scratch.worktree = Some(worktree.clone());
    let build_started = Instant::now();
    let old = build(&root, &worktree, &place.join("target"))?;
    let build_secs = build_started.elapsed().as_secs_f64();

    // Repeats alternate the sides: REF first in even ones, the working tree
    // first in odd ones.
    let mut tasks = Vec::new();
    for repeat in 0..options.repeats.max(1) {
        for workload in 0..names.len() {
            let sides = if repeat.is_multiple_of(2) {
                [0, 1]
            } else {
                [1, 0]
            };
            for side in sides {
                for baseline in [false, true] {
                    tasks.push(Task {
                        workload,
                        side,
                        baseline,
                    });
                }
            }
        }
    }
    eprintln!(
        "{} workloads, {} runs on {} threads, counted by {}",
        names.len(),
        tasks.len(),
        options.jobs,
        counter.name()
    );
    let measure_started = Instant::now();
    let results = count_all(counter, &[old, new], &names, &corpus, &tasks, options.jobs);
    let measure_secs = measure_started.elapsed().as_secs_f64();

    // Each workload's counts by side, runs and baselines.
    let mut counts = vec![[[Vec::new(), Vec::new()], [Vec::new(), Vec::new()]]; names.len()];
    let mut paints: Vec<[Option<(u64, u64)>; 2]> = vec![[None, None]; names.len()];
    for (task, result) in tasks.iter().zip(&results) {
        let (n, stdout) = result.as_ref().map_err(Clone::clone)?;
        let n = i64::try_from(*n).unwrap_or(i64::MAX);
        if let Some(list) = counts
            .get_mut(task.workload)
            .and_then(|c| c.get_mut(task.side))
            .and_then(|c| c.get_mut(usize::from(task.baseline)))
        {
            list.push(n);
        }
        if let Some(paint) = paints
            .get_mut(task.workload)
            .and_then(|p| p.get_mut(task.side))
            && !task.baseline
        {
            *paint = painted(stdout).filter(|(frames, _)| *frames > 0);
        }
    }

    let mut table = format!(
        "instructions retired, {} (REF {short}) vs the working tree ({}{}); the fewest of {} \
         runs each, less the fewest of its baseline\n{:<26} {:>15} {:>15} {:>8} {:>7}\n",
        options.reference,
        head.get(..12).unwrap_or(&head),
        if dirty { ", with changes" } else { "" },
        options.repeats,
        "workload",
        "REF",
        "working tree",
        "change",
        "noise"
    );
    let mut rows = Vec::new();
    let mut flagged = Vec::new();
    let mut noisy = Vec::new();
    let mut worst_noise = 0f64;
    for (i, spec) in specs.iter().enumerate() {
        let Some([[old_full, old_base], [new_full, new_base]]) = counts.get(i) else {
            continue;
        };
        let (old, new) = (side(old_full, old_base), side(new_full, new_base));
        let change = (new.count as f64) / (old.count.max(1) as f64) - 1.0;
        let noise = old.noise.max(new.noise);
        worst_noise = worst_noise.max(noise);
        let flag = change * 100.0 > options.threshold && change > noise;
        if flag {
            flagged.push(spec.name.clone());
        }
        if noise * 100.0 > options.threshold {
            noisy.push(spec.name.clone());
        }
        let _ = writeln!(
            table,
            "{:<26} {:>15} {:>15} {:>+7.2}% {:>6.2}%{}",
            spec.name,
            old.count,
            new.count,
            change * 100.0,
            noise * 100.0,
            if flag { "  <- slower" } else { "" }
        );
        let paint = paints.get(i).copied().unwrap_or_default();
        let per_frame = |p: Option<(u64, u64)>| {
            p.map(|(frames, bytes)| {
                serde_json::json!({
                    "frames": frames,
                    "bytes": bytes,
                    "bytes_per_frame": (bytes as f64) / (frames.max(1) as f64),
                })
            })
        };
        rows.push(serde_json::json!({
            "name": spec.name,
            "about": spec.about,
            "ref": old.count,
            "working_tree": new.count,
            "change_percent": change * 100.0,
            "ref_noise_percent": old.noise * 100.0,
            "working_tree_noise_percent": new.noise * 100.0,
            "flagged": flag,
            "ref_paint": per_frame(paint.first().copied().flatten()),
            "working_tree_paint": per_frame(paint.get(1).copied().flatten()),
        }));
    }
    let total_secs = started.elapsed().as_secs_f64();
    let _ = writeln!(
        table,
        "\nthe largest run-to-run spread of one side: {:.2}%; flagged above {}% and the \
         noise: {}\nbuilt REF in {build_secs:.0} s, counted in {measure_secs:.0} s, \
         {total_secs:.0} s in all",
        worst_noise * 100.0,
        options.threshold,
        if flagged.is_empty() {
            "none".to_owned()
        } else {
            flagged.join(", ")
        }
    );
    if !noisy.is_empty() {
        let _ = writeln!(
            table,
            "too noisy here to judge at {}% (more --repeats, or a quieter machine): {}",
            options.threshold,
            noisy.join(", ")
        );
    }
    print!("{table}");
    let json = serde_json::json!({
        "kind": "fux-bench against",
        "ref": options.reference,
        "ref_commit": reference,
        "working_tree_commit": head,
        "working_tree_changed": dirty,
        "counter": counter.name(),
        "repeats": options.repeats,
        "jobs": options.jobs,
        "threshold_percent": options.threshold,
        "worst_noise_percent": worst_noise * 100.0,
        "flagged": flagged,
        "noisy": noisy,
        "seconds": { "build_ref": build_secs, "count": measure_secs, "total": total_secs },
        "workloads": rows,
    });
    crate::save_json(&options.json, &json)?;
    eprintln!("wrote {}", options.json.display());
    Ok(flagged.is_empty())
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_side_is_its_fewest_less_its_baselines_fewest() {
        let side = super::side(&[1103, 1100, 1101], &[101, 100, 102]);
        assert_eq!(side.count, 1000);
        assert!((side.noise - 3.0 / 1000.0).abs() < 1e-9);
    }

    #[test]
    fn paint_statistics_are_read_from_a_run() {
        assert_eq!(
            super::painted("paint/corpus 4194304 1200 345678\n"),
            Some((1200, 345_678))
        );
        assert_eq!(super::painted("vt/ascii 4194304 0 0\n"), Some((0, 0)));
        assert_eq!(super::painted(""), None);
    }
}
