//! `--against REF`: every workload on REF and on the working tree, in
//! instructions retired.
//!
//! Both sides are built by one recipe (`build`): each side's tree is
//! `git archive`d into a path of one length (`src/old-TREE`,
//! `src/new-TREE`), built with fux's release profile into one target dir,
//! and run from names of one length (`run/old`, `run/new`). REF's side is
//! REF's tree with the working tree's `bench/` in it, so both sides run
//! the same workloads, each linked against its own fux, fux-vt and fuxix
//! (one binary cannot link two copies of a crate of one version). The
//! working tree's side is the working tree as it is, changes and untracked
//! files included. A path of another length moves the counts (by 1% on
//! `ready/probe`); one recipe for both keeps an A/A run, `--against HEAD`
//! on a clean tree, at no difference.
//!
//! Each workload runs as a child process of its own, counted (`count`),
//! and so does its baseline (`workloads`): in each of `--repeats` rounds,
//! every workload's four runs (each side, and each side's baseline), the
//! side that goes first alternating by round. The judgement is
//! `speed-method.md`'s: a round's change pairs both sides' runs in it, each
//! less its side's median baseline; a workload's change is the median of
//! its rounds', its spread their median absolute deviation. Its floor comes
//! from a calibration (`--calibrate`, an A/A run of its own, `--floors`
//! FILE): max(0.25%, the A/A change, 3 x the A/A spread). A change inside
//! its floor is inconclusive; a floor over 1% means the tool is broken for
//! that workload; a run whose spread is past two thirds of the floor
//! (twice what the floor allows for) is noisy. Across the recordings, a
//! sign test with the recordings of one program counted once flags a broad
//! change.
use crate::count::Counter;
use crate::workloads;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

pub struct Options {
    pub reference: String,
    pub repeats: usize,
    pub jobs: usize,
    /// A calibration's JSON (`--calibrate`) to take floors from.
    pub floors: Option<PathBuf>,
    /// Take them only if made here (`--floors-here`): one made by another
    /// counter or machine is said and left out, each workload judged by
    /// this run's spread; else (`--floors`) it stops the run.
    pub floors_here: bool,
    /// An A/A run of the working tree against itself, writing floors.
    pub calibrate: bool,
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

/// `git` with a temporary index of its own at `index`.
fn git_index(root: &Path, index: &Path, args: &[&str]) -> Result<String, String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .env("GIT_INDEX_FILE", index)
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

/// The working tree as a git tree: HEAD with every change in it, tracked
/// or not (but ignored files), written with an index of its own, so the
/// checkout's index and stash are untouched.
fn working_tree(root: &Path, place: &Path) -> Result<String, String> {
    let index = place.join("index");
    let _ = std::fs::remove_file(&index);
    git_index(root, &index, &["read-tree", "HEAD"])?;
    git_index(root, &index, &["add", "-A", "."])?;
    let tree = git_index(root, &index, &["write-tree"]);
    let _ = std::fs::remove_file(&index);
    tree
}

/// `commit`'s tree with its `bench/` replaced by `bench`'s: REF's crates
/// under the working tree's workloads. The removal is forced (`-f`): git
/// refuses to drop staged content that differs from both the file and
/// HEAD, as REF's `bench/` does whenever both changed it since.
fn with_bench(root: &Path, place: &Path, commit: &str, bench: &str) -> Result<String, String> {
    let index = place.join("index");
    let _ = std::fs::remove_file(&index);
    git_index(root, &index, &["read-tree", commit])?;
    git_index(
        root,
        &index,
        &[
            "rm",
            "-r",
            "-q",
            "-f",
            "--cached",
            "--ignore-unmatch",
            "bench",
        ],
    )?;
    git_index(root, &index, &["read-tree", "--prefix=bench/", bench])?;
    let tree = git_index(root, &index, &["write-tree"]);
    let _ = std::fs::remove_file(&index);
    tree
}

/// The two sides: REF is `old`, the working tree `new`. Their names have
/// one length, so their sources, and the binaries run, sit at paths of
/// one length (a path of another length moves the counts).
const SIDES: [&str; 2] = ["old", "new"];

/// Builds `tree` as side `side`, by the one recipe both sides share:
/// `git archive` into `place/src/SIDE-TREE`, built with fux's release
/// profile into the one target dir `place/target`, from the source's own
/// `bench/` so its toolchain file applies, and the binary kept as
/// `place/bin/SIDE-TREE`. A binary kept from an earlier run is reused, and
/// one of the other side's name too unless both sides are this tree (an
/// A/A run builds twice). The source is removed after its build.
fn build(
    root: &Path,
    place: &Path,
    side: &str,
    (tree, other): (&str, &str),
    jobs: usize,
) -> Result<PathBuf, String> {
    let bin = place.join("bin");
    std::fs::create_dir_all(&bin).map_err(|e| format!("{}: {e}", bin.display()))?;
    let kept = bin.join(format!("{side}-{tree}"));
    if kept.exists() {
        return Ok(kept);
    }
    if tree != other {
        for name in SIDES.iter().filter(|s| **s != side) {
            let theirs = bin.join(format!("{name}-{tree}"));
            if theirs.exists() {
                return Ok(theirs);
            }
        }
    }
    let src = place.join("src").join(format!("{side}-{tree}"));
    let _ = std::fs::remove_dir_all(&src);
    std::fs::create_dir_all(&src).map_err(|e| format!("{}: {e}", src.display()))?;
    let mut archive = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["archive", "--format=tar", tree])
        .stdout(Stdio::piped())
        .spawn()
        .map_err(|e| format!("git archive: {e}"))?;
    let tar = Command::new("tar")
        .arg("-x")
        .arg("-C")
        .arg(&src)
        .stdin(archive.stdout.take().ok_or("git archive: no output")?)
        .status()
        .map_err(|e| format!("tar: {e}"))?;
    let archived = archive.wait().map_err(|e| format!("git archive: {e}"))?;
    if !tar.success() || !archived.success() {
        return Err(format!("git archive {tree} into {} failed", src.display()));
    }
    let bench = src.join("bench");
    let out = Command::new("cargo")
        .args([
            "build",
            "--release",
            "--quiet",
            "-j",
            &jobs.to_string(),
            "--manifest-path",
        ])
        .arg(bench.join("Cargo.toml"))
        .arg("--target-dir")
        .arg(place.join("target"))
        .env_remove("CARGO_TARGET_DIR")
        .current_dir(&bench)
        .output()
        .map_err(|e| format!("cargo: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "building {side} ({tree}) failed:\n{}",
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    let built = place.join("target/release/fux-bench");
    std::fs::copy(&built, &kept).map_err(|e| format!("{}: {e}", built.display()))?;
    let _ = std::fs::remove_dir_all(&src);
    Ok(kept)
}

/// Where `against` keeps its builds: `$CARGO_TARGET_DIR/fux-bench-against`
/// if that is set, else `bench/target/against`.
fn place(root: &Path) -> PathBuf {
    std::env::var_os("CARGO_TARGET_DIR").map_or_else(
        || root.join("bench/target/against"),
        |dir| PathBuf::from(dir).join("fux-bench-against"),
    )
}

/// A run's count and what it printed, or why it failed.
type Counted = Result<crate::count::Run, String>;

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
    let payloads = crate::census::data(&root());
    let payloads = payloads.to_string_lossy();
    let done = AtomicUsize::new(0);
    let results: Vec<(usize, Counted)> = std::thread::scope(|scope| {
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
                        let mut args =
                            vec!["run", name, "--corpus", &corpus, "--payloads", &payloads];
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
    // Each result in its task's place: a worker that panicked leaves its
    // tasks uncounted, never another task's result in their place.
    let mut slots: Vec<Counted> = (tasks.iter())
        .map(|_| Err("not counted: its worker stopped".to_owned()))
        .collect();
    for (i, result) in results {
        if let Some(slot) = slots.get_mut(i) {
            *slot = result;
        }
    }
    slots
}

/// The floor no workload's goes under, and the floor past which the tool
/// is broken for a workload (`speed-method.md`).
const FLOOR_MIN: f64 = 0.0025;
const FLOOR_BROKEN: f64 = 0.01;
/// A program's recordings count in the sign test only when their median
/// change is past this, either way.
const SIGN_BAND: f64 = 0.001;
/// The sign test flags a broad change at this one-sided p or under.
const SIGN_P: f64 = 0.01;

fn median(values: &[f64]) -> f64 {
    let mut v: Vec<f64> = values.iter().copied().filter(|x| x.is_finite()).collect();
    v.sort_by(f64::total_cmp);
    let n = v.len();
    match (v.get(n / 2), n.checked_rem(2)) {
        (Some(&high), Some(0)) => v
            .get(n.saturating_sub(1) / 2)
            .map_or(high, |&low| f64::midpoint(low, high)),
        (Some(&mid), _) => mid,
        (None, _) => f64::NAN,
    }
}

/// One workload's rounds, judged: each round pairs both sides' runs in it,
/// `change` is the median of the rounds' changes, `spread` their median
/// absolute deviation from it. A side's run counts less its baseline's
/// median (the baseline is the same work with the measured part left out).
#[derive(Debug, PartialEq)]
struct Paired {
    /// Each side's median count less its baseline's median.
    old: f64,
    new: f64,
    rounds: Vec<f64>,
    change: f64,
    spread: f64,
}

fn paired(old: [&[i64]; 2], new: [&[i64]; 2]) -> Paired {
    let floats = |v: &[i64]| v.iter().map(|&n| n as f64).collect::<Vec<_>>();
    let [old_full, old_base] = old.map(floats);
    let [new_full, new_base] = new.map(floats);
    let (old_base, new_base) = (median(&old_base), median(&new_base));
    let rounds: Vec<f64> = old_full
        .iter()
        .zip(&new_full)
        .map(|(o, n)| (n - new_base) / (o - old_base) - 1.0)
        .collect();
    // A run at or below its side's baseline counts nothing that can be
    // compared: the workload's change is none, and it is judged broken.
    let counted =
        (old_full.iter()).all(|o| *o > old_base) && new_full.iter().all(|n| *n > new_base);
    let change = if counted { median(&rounds) } else { f64::NAN };
    let deviations: Vec<f64> = rounds.iter().map(|r| (r - change).abs()).collect();
    Paired {
        old: median(&old_full) - old_base,
        new: median(&new_full) - new_base,
        spread: median(&deviations),
        change,
        rounds,
    }
}

/// What a comparison says of a workload.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Verdict {
    Slower,
    Faster,
    /// Within its floor.
    Inconclusive,
    /// This run's spread is past two thirds of its calibrated floor (twice
    /// the spread the floor allows for): it says nothing.
    Noisy,
    /// Its floor is over 1%: the tool cannot judge it.
    Broken,
}

impl Verdict {
    fn name(self) -> &'static str {
        match self {
            Verdict::Slower => "slower",
            Verdict::Faster => "faster",
            Verdict::Inconclusive => "inconclusive",
            Verdict::Noisy => "noisy",
            Verdict::Broken => "broken",
        }
    }
}

/// A workload's floor from an A/A calibration: max(0.25%, the A/A change,
/// 3 x the A/A spread).
fn floor(aa: &Paired) -> f64 {
    FLOOR_MIN.max(aa.change.abs()).max(3.0 * aa.spread)
}

/// The floor `p` is judged against, and whether it was calibrated: the
/// floors file's, if it has one for the workload; a calibration's own
/// (`floor`); else this run's, max(0.25%, 3 x its spread), which its own
/// change takes no part in, or no change would ever be slower.
fn floor_of(p: &Paired, from_file: Option<f64>, calibrating: bool) -> (f64, bool) {
    match from_file {
        Some(floor) => (floor, true),
        None if calibrating => (floor(p), true),
        None => (FLOOR_MIN.max(3.0 * p.spread), false),
    }
}

/// `p` judged against `floor`; `calibrated` if the floor came from an
/// A/A run apart from this one.
fn verdict(p: &Paired, floor: f64, calibrated: bool) -> Verdict {
    if floor > FLOOR_BROKEN || !p.change.is_finite() {
        Verdict::Broken
    } else if calibrated && 3.0 * p.spread > 2.0 * floor {
        Verdict::Noisy
    } else if p.change > floor {
        Verdict::Slower
    } else if p.change < -floor {
        Verdict::Faster
    } else {
        Verdict::Inconclusive
    }
}

/// The program a recording workload belongs to (`vt/corpus:nvim-diff` is
/// nvim's), or none for a workload that is not a recording.
fn program(name: &str) -> Option<&str> {
    let recording = name.strip_prefix("vt/corpus:")?;
    Some(recording.split_once('-').map_or(recording, |(p, _)| p))
}

/// The sign test over the recordings, a program's recordings counted as
/// one (their median change): how many programs are past `SIGN_BAND`
/// slower and faster, and the one-sided p of so many slower (and of so
/// many faster) if each way were as likely.
#[derive(Debug, PartialEq)]
struct Signs {
    programs: usize,
    slower: usize,
    faster: usize,
    p_slower: f64,
    p_faster: f64,
}

fn signs(changes: &[(&str, f64)]) -> Signs {
    let mut by: std::collections::BTreeMap<&str, Vec<f64>> = std::collections::BTreeMap::new();
    for &(name, change) in changes {
        if let Some(p) = program(name) {
            by.entry(p).or_default().push(change);
        }
    }
    let medians: Vec<f64> = by.values().map(|v| median(v)).collect();
    let slower = medians.iter().filter(|&&m| m > SIGN_BAND).count();
    let faster = medians.iter().filter(|&&m| m < -SIGN_BAND).count();
    let n = slower.saturating_add(faster);
    Signs {
        programs: medians.len(),
        slower,
        faster,
        p_slower: tail(n, slower),
        p_faster: tail(n, faster),
    }
}

/// P(X >= k) for X binomial(n, 1/2).
fn tail(n: usize, k: usize) -> f64 {
    let mut choose = 1f64;
    let mut sum = 0f64;
    for i in 0..=n {
        if i >= k {
            sum += choose;
        }
        choose = choose * (n.saturating_sub(i) as f64) / (i.saturating_add(1) as f64);
    }
    sum / 2f64.powi(i32::try_from(n).unwrap_or(i32::MAX))
}

/// Floors from a calibration's JSON: each workload's floor, by name.
fn read_floors(
    path: &Path,
    (counter, setup): (&str, &str),
) -> Result<std::collections::BTreeMap<String, f64>, Unfit> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| Unfit::Unread(format!("{}: {e}", path.display())))?;
    let json: serde_json::Value = serde_json::from_str(&text)
        .map_err(|e| Unfit::Unread(format!("{}: {e}", path.display())))?;
    floors_of(&json, (counter, setup)).map_err(|e| match e {
        Unfit::Unread(why) => Unfit::Unread(format!("{}: {why}", path.display())),
        Unfit::Elsewhere(why) => Unfit::Elsewhere(format!("{}: {why}", path.display())),
    })
}

/// Why floors can't be taken from a file: it can't be read as a
/// calibration, or it is one made by another counter or on another
/// machine.
#[derive(Debug)]
enum Unfit {
    Unread(String),
    Elsewhere(String),
}

/// The machine a count was made on, as its counter's report says: its
/// PMU and CPU lines (`perf.sh`'s `pmu NAME CPUS` and `cpu N MODEL`).
fn machine(setup: &str) -> Vec<&str> {
    (setup.lines())
        .filter(|l| l.starts_with("pmu ") || l.starts_with("cpu "))
        .collect()
}

/// Each workload's floor in a calibration's JSON, if it was made by the
/// counter this run counts with (`counter`, its report `setup`) on this
/// machine: a floor is a spread of one counter on one machine.
fn floors_of(
    json: &serde_json::Value,
    (counter, setup): (&str, &str),
) -> Result<std::collections::BTreeMap<String, f64>, Unfit> {
    let field = |name: &str| json.get(name).and_then(serde_json::Value::as_str);
    if field("kind") != Some("fux-bench calibration") {
        return Err(Unfit::Unread("not a calibration (--calibrate)".into()));
    }
    if field("counter") != Some(counter) {
        return Err(Unfit::Elsewhere(format!(
            "calibrated by {}, and this run counts by {counter}",
            field("counter").unwrap_or("an unnamed counter")
        )));
    }
    if machine(field("counter_setup").unwrap_or_default()) != machine(setup) {
        return Err(Unfit::Elsewhere(
            "calibrated on another machine: its CPUs or PMUs are not this one's".into(),
        ));
    }
    let rows = json
        .get("workloads")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| Unfit::Unread("no workloads".into()))?;
    Ok(rows
        .iter()
        .filter_map(|w| {
            let name = w.get("name")?.as_str()?;
            let floor = w.get("floor_percent")?.as_f64()? / 100.0;
            Some((name.to_owned(), floor))
        })
        .collect())
}

/// The paint statistics a run prints: frames, and bytes painted.
fn painted(stdout: &str) -> Option<(u64, u64)> {
    let mut words = stdout.split_whitespace();
    let frames = words.nth(2)?.parse().ok()?;
    let bytes = words.next()?.parse().ok()?;
    Some((frames, bytes))
}

pub fn run(options: &Options) -> Result<bool, String> {
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
    let (counter, counter_setup) = Counter::detect()?;
    // Read before the counts, so a floors file that can't be used stops
    // the run at once.
    let floors = match options.floors.as_deref() {
        None => None,
        Some(path) => match read_floors(path, (counter.name(), &counter_setup)) {
            Ok(floors) => Some(floors),
            // Said, never left out silently: the table's header says how
            // each workload is judged instead.
            Err(Unfit::Elsewhere(why)) if options.floors_here => {
                eprintln!("fux-bench: {why}; judging each workload by this run's spread instead");
                None
            }
            Err(Unfit::Unread(why) | Unfit::Elsewhere(why)) => return Err(why),
        },
    };
    let floors_from = floors.as_ref().and(options.floors.as_ref());
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

    let place = place(&root);
    std::fs::create_dir_all(&place).map_err(|e| format!("{}: {e}", place.display()))?;
    let new_tree = working_tree(&root, &place)?;
    let bench_tree = git(&root, &["rev-parse", &format!("{new_tree}:bench")])?;
    // A calibration is an A/A run: the working tree on both sides.
    let old_tree = if options.calibrate {
        new_tree.clone()
    } else {
        with_bench(&root, &place, &reference, &bench_tree)?
    };
    let short = reference.get(..12).unwrap_or(&reference).to_owned();
    eprintln!(
        "building {} ({short}) and the working tree by one recipe, in {}…",
        options.reference,
        place.display()
    );
    let build_started = Instant::now();
    let old = build(&root, &place, "old", (&old_tree, &new_tree), options.jobs)?;
    let new = build(&root, &place, "new", (&new_tree, &old_tree), options.jobs)?;
    let build_secs = build_started.elapsed().as_secs_f64();
    // Run from names of one length, copied, so a build meanwhile cannot
    // change them.
    let run = place.join("run");
    let _ = std::fs::remove_dir_all(&run);
    std::fs::create_dir_all(&run).map_err(|e| format!("{}: {e}", run.display()))?;
    let mut binaries = SIDES.map(|side| run.join(side));
    for (from, to) in [&old, &new].into_iter().zip(&mut binaries) {
        std::fs::copy(from, &*to).map_err(|e| format!("{}: {e}", from.display()))?;
    }
    let [old_binary, new_binary] = binaries;

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
    let results = count_all(
        counter,
        &[old_binary.clone(), new_binary.clone()],
        &names,
        &corpus,
        &tasks,
        options.jobs,
    );
    let measure_secs = measure_started.elapsed().as_secs_f64();

    // Each workload's counts by side, runs and baselines.
    let mut counts = vec![[[Vec::new(), Vec::new()], [Vec::new(), Vec::new()]]; names.len()];
    let mut paints: Vec<[Option<(u64, u64)>; 2]> = vec![[None, None]; names.len()];
    // perf's own report of every run, kept beside the JSON.
    let mut raw = format!("{counter_setup}\n");
    for (i, (task, result)) in tasks.iter().zip(&results).enumerate() {
        let run = result.as_ref().map_err(Clone::clone)?;
        let report = run
            .raw
            .rsplit_once("fux-perf: begin\n")
            .map_or(run.raw.as_str(), |(_, block)| block);
        let _ = writeln!(
            raw,
            "## run {i}: {} {}{}\n{}",
            SIDES.get(task.side).unwrap_or(&"?"),
            names.get(task.workload).map_or("?", String::as_str),
            if task.baseline { " --baseline" } else { "" },
            report.trim_end()
        );
        let (n, stdout) = (
            i64::try_from(run.instructions).unwrap_or(i64::MAX),
            &run.stdout,
        );
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

    let calibrating = options.calibrate;
    let mut table = format!(
        "instructions retired, {} (REF {short}) vs the working tree ({}{}): the median of {} \
         interleaved paired rounds, each run less its side's median baseline; floors {}\n\
         {:<40} {:>14} {:>14} {:>8} {:>7} {:>7}  verdict\n",
        options.reference,
        head.get(..12).unwrap_or(&head),
        if dirty { ", with changes" } else { "" },
        options.repeats,
        if calibrating {
            "calibrated here: max(0.25%, |A/A change|, 3 x spread)".to_owned()
        } else if let Some(path) = floors_from {
            format!("from {}", path.display())
        } else {
            "uncalibrated: max(0.25%, 3 x this run's spread)".to_owned()
        },
        "workload",
        "REF",
        "working tree",
        "change",
        "spread",
        "floor",
    );
    let mut rows = Vec::new();
    let mut judged: Vec<(String, Verdict)> = Vec::new();
    let mut changes: Vec<(String, f64)> = Vec::new();
    for (i, spec) in specs.iter().enumerate() {
        let Some([[old_full, old_base], [new_full, new_base]]) = counts.get(i) else {
            continue;
        };
        let p = paired([old_full, old_base], [new_full, new_base]);
        let from_file = floors.as_ref().and_then(|f| f.get(&spec.name)).copied();
        let (floor, calibrated) = floor_of(&p, from_file, calibrating);
        let verdict = if calibrating {
            if floor > FLOOR_BROKEN {
                Verdict::Broken
            } else {
                Verdict::Inconclusive
            }
        } else {
            verdict(&p, floor, calibrated)
        };
        judged.push((spec.name.clone(), verdict));
        changes.push((spec.name.clone(), p.change));
        let _ = writeln!(
            table,
            "{:<40} {:>14.0} {:>14.0} {:>+7.3}% {:>6.3}% {:>6.3}%  {}",
            spec.name,
            p.old,
            p.new,
            p.change * 100.0,
            p.spread * 100.0,
            floor * 100.0,
            verdict.name()
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
            "fed": workloads::fed(&spec.name),
            "ref": p.old,
            "working_tree": p.new,
            "change_percent": p.change * 100.0,
            "spread_percent": p.spread * 100.0,
            "floor_percent": floor * 100.0,
            "floor_calibrated": calibrated,
            "verdict": verdict.name(),
            "rounds_percent": p.rounds.iter().map(|r| r * 100.0).collect::<Vec<_>>(),
            "counts": {
                "ref": old_full, "ref_baseline": old_base,
                "working_tree": new_full, "working_tree_baseline": new_base,
            },
            "ref_paint": per_frame(paint.first().copied().flatten()),
            "working_tree_paint": per_frame(paint.get(1).copied().flatten()),
        }));
    }
    let by_name: Vec<(&str, f64)> = changes.iter().map(|(n, c)| (n.as_str(), *c)).collect();
    let signs = signs(&by_name);
    let broad_loss = !calibrating && signs.p_slower <= SIGN_P;
    let broad_gain = !calibrating && signs.p_faster <= SIGN_P;
    let named = |v: Verdict| -> Vec<&str> {
        judged
            .iter()
            .filter(|(_, x)| *x == v)
            .map(|(n, _)| n.as_str())
            .collect()
    };
    let list = |v: Vec<&str>| {
        if v.is_empty() {
            "none".to_owned()
        } else {
            v.join(", ")
        }
    };
    let total_secs = started.elapsed().as_secs_f64();
    let _ = writeln!(
        table,
        "\nslower past its floor: {}\nfaster past its floor: {}\nnoisy: {}\nbroken (floor over 1%): {}\n\
         recordings by program: {} programs, {} slower and {} faster past {:.1}%; one-sided sign \
         test p {:.4} slower, {:.4} faster{}\n\
         built in {build_secs:.0} s, counted in {measure_secs:.0} s, {total_secs:.0} s in all",
        list(named(Verdict::Slower)),
        list(named(Verdict::Faster)),
        list(named(Verdict::Noisy)),
        list(named(Verdict::Broken)),
        signs.programs,
        signs.slower,
        signs.faster,
        SIGN_BAND * 100.0,
        signs.p_slower,
        signs.p_faster,
        if broad_loss {
            ": a broad loss"
        } else if broad_gain {
            ": a broad gain"
        } else {
            ""
        },
    );
    print!("{table}");
    let json = serde_json::json!({
        "kind": if calibrating { "fux-bench calibration" } else { "fux-bench against" },
        "method": "speed-method.md: the median of interleaved paired rounds; floor max(0.25%, A/A change, 3 x A/A spread); over 1% broken; sign test by program",
        "ref": options.reference,
        "ref_commit": reference,
        "working_tree_commit": head,
        "working_tree_changed": dirty,
        "trees": { "old": old_tree, "new": new_tree },
        "recipe": "git archive into paths of one length, one target dir, run as run/old and run/new",
        "binaries": { "old": hash(&old_binary)?, "new": hash(&new_binary)? },
        "counter": counter.name(),
        "counter_setup": counter_setup,
        "rounds": options.repeats,
        "jobs": options.jobs,
        "floors": floors_from.map(|p| p.display().to_string()),
        "slower": named(Verdict::Slower),
        "faster": named(Verdict::Faster),
        "noisy": named(Verdict::Noisy),
        "broken": named(Verdict::Broken),
        "signs": {
            "programs": signs.programs, "slower": signs.slower, "faster": signs.faster,
            "band_percent": SIGN_BAND * 100.0, "p_slower": signs.p_slower,
            "p_faster": signs.p_faster, "broad_loss": broad_loss, "broad_gain": broad_gain,
        },
        "seconds": { "build": build_secs, "count": measure_secs, "total": total_secs },
        "workloads": rows,
    });
    crate::save_json(&options.json, &json)?;
    let raw_path = options.json.with_extension("perf.txt");
    std::fs::write(&raw_path, raw).map_err(|e| format!("{}: {e}", raw_path.display()))?;
    eprintln!(
        "wrote {} and {}",
        options.json.display(),
        raw_path.display()
    );
    if !named(Verdict::Broken).is_empty() {
        return Err("the tool cannot judge every workload: see broken above".into());
    }
    Ok(named(Verdict::Slower).is_empty() && !broad_loss)
}

/// The programs a file's SHA-256 is asked of, in turn (some systems have
/// only one of them).
const HASHERS: [(&str, &[&str]); 2] = [("sha256sum", &[]), ("shasum", &["-a", "256"])];

/// A file's SHA-256, by the first of `HASHERS` that gives one.
fn hash(path: &Path) -> Result<String, String> {
    hash_by(path, &HASHERS)
}

/// A file's SHA-256, by the first of `programs` that exits 0 with 64 hex
/// digits; else why none did.
fn hash_by(path: &Path, programs: &[(&str, &[&str])]) -> Result<String, String> {
    let mut why = Vec::new();
    for (program, args) in programs {
        let out = match Command::new(program).args(*args).arg(path).output() {
            Ok(out) => out,
            Err(e) => {
                why.push(format!("{program}: {e}"));
                continue;
            }
        };
        let text = String::from_utf8_lossy(&out.stdout);
        let hash = text.split_whitespace().next().unwrap_or_default();
        if out.status.success() && hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Ok(hash.to_owned());
        }
        why.push(format!("{program}: no hash ({})", out.status));
    }
    Err(format!(
        "{}: no SHA-256: {}",
        path.display(),
        why.join("; ")
    ))
}

#[cfg(test)]
mod tests {
    use super::{
        FLOOR_MIN, Unfit, Verdict, floor_of, floors_of, hash_by, paired, signs, tail, verdict,
    };

    /// A floors file is used only for the counter and the machine it was
    /// calibrated with: a calibration by another counter, or on CPUs or
    /// PMUs not this machine's, is refused.
    #[test]
    fn floors_are_of_one_counter_on_one_machine() {
        let setup =
            "perf version 7.2.3\ncpus all\naslr off\ncpu 0 Intel i5-9600K\ncpu 1 Intel i5-9600K";
        let calibration = |counter: &str, setup: &str| {
            serde_json::json!({
                "kind": "fux-bench calibration", "counter": counter, "counter_setup": setup,
                "workloads": [{ "name": "vt/ascii", "floor_percent": 0.3 }],
            })
        };
        let perf = "bench/perf.sh (perf stat -e instructions:u, pinned, unscaled)";
        let floors = floors_of(&calibration(perf, setup), (perf, setup));
        assert!(floors.is_ok_and(|f| f.get("vt/ascii").is_some_and(|f| (f - 0.003).abs() < 1e-12)));
        // perf of another version on the same CPUs: the same machine.
        let newer = setup.replace("7.2.3", "7.3.0");
        assert!(floors_of(&calibration(perf, &newer), (perf, setup)).is_ok());
        let time = "/usr/bin/time -l (instructions retired)";
        let elsewhere = |r| matches!(r, Err(Unfit::Elsewhere(_)));
        assert!(elsewhere(floors_of(
            &calibration(time, setup),
            (perf, setup)
        )));
        let other = "perf version 7.2.3\ncpus all\naslr off\ncpu 0 Apple M4\ncpu 1 Apple M4";
        assert!(elsewhere(floors_of(
            &calibration(perf, other),
            (perf, setup)
        )));
        let against = serde_json::json!({ "kind": "fux-bench against" });
        assert!(matches!(
            floors_of(&against, (perf, setup)),
            Err(Unfit::Unread(_))
        ));
    }

    /// A file's SHA-256 is asked of each program in turn: one that fails,
    /// is missing, or prints no 64 hex digits (`true`) is passed over
    /// (FIPS 180-2's example "abc").
    #[test]
    fn a_file_s_hash_falls_through_to_a_program_that_gives_one() -> Result<(), String> {
        let path = std::env::temp_dir().join(format!("fux-bench-hash-{}", std::process::id()));
        std::fs::write(&path, b"abc").map_err(|e| e.to_string())?;
        let abc = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
        let programs = [
            ("false", &[][..]),
            ("true", &[]),
            ("no-such-hasher", &[]),
            ("sha256sum", &[]),
        ];
        let hashed = hash_by(&path, &programs);
        let refused = hash_by(&path, &[("false", &[]), ("true", &[])]);
        let _ = std::fs::remove_file(&path);
        assert_eq!(hashed?, abc);
        assert!(refused.is_err());
        Ok(())
    }
    use std::path::Path;
    use std::process::Command;

    /// Runs `git args` in `dir`, and its output.
    fn git(dir: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["-c", "user.name=t", "-c", "user.email=t@t"])
            .args(args)
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned());
        assert!(out.is_some(), "git {args:?}");
        out.unwrap_or_default()
    }

    /// A REF whose `bench/` differs from both HEAD's and the working
    /// tree's gets the working tree's, the rest of REF's tree kept: git
    /// refused this removal unless forced (`9a38df95`).
    #[test]
    fn a_ref_whose_bench_differs_from_head_and_the_tree_takes_the_trees() {
        let base =
            std::env::temp_dir().join(format!("fux-bench-with-bench-{}", std::process::id()));
        let (root, place) = (base.join("repo"), base.join("place"));
        let _ = std::fs::remove_dir_all(&base);
        let made = std::fs::create_dir_all(root.join("bench")).and(std::fs::create_dir_all(&place));
        assert!(made.is_ok());
        let write = |path: &str, text: &str| assert!(std::fs::write(root.join(path), text).is_ok());
        git(&root, &["init", "-q"]);
        write("bench/w.rs", "ref");
        write("crate.rs", "ref's crate");
        git(&root, &["add", "-A"]);
        git(&root, &["commit", "-q", "-m", "ref"]);
        let reference = git(&root, &["rev-parse", "HEAD"]);
        write("bench/w.rs", "head");
        git(&root, &["commit", "-q", "-am", "head"]);
        write("bench/w.rs", "working tree");
        let tree = super::working_tree(&root, &place);
        let bench = tree.and_then(|t| super::git(&root, &["rev-parse", &format!("{t}:bench")]));
        let made = bench.and_then(|b| super::with_bench(&root, &place, &reference, &b));
        assert!(made.is_ok(), "{made:?}");
        let made = made.unwrap_or_default();
        assert_eq!(
            git(&root, &["show", &format!("{made}:bench/w.rs")]),
            "working tree"
        );
        assert_eq!(
            git(&root, &["show", &format!("{made}:crate.rs")]),
            "ref's crate"
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn a_change_is_the_median_of_paired_rounds() {
        // Baselines' medians 100 each side; rounds less them 1000/1010,
        // 1000/1020, 1100/1110: changes +1%, +2%, +1/110; median +1%,
        // spread the median of 0, 1% and 1% - 1/110, which is 1/1100.
        let p = paired(
            [&[1100, 1100, 1200], &[100, 99, 101]],
            [&[1110, 1120, 1210], &[100, 100, 100]],
        );
        assert!((p.change - 0.01).abs() < 1e-12, "{p:?}");
        assert!((p.spread - 1.0 / 1100.0).abs() < 1e-12, "{p:?}");
        assert!((p.old - 1000.0).abs() < 1e-9 && (p.new - 1020.0).abs() < 1e-9);
    }

    /// Without a floor from a file, a run is judged by its own spread:
    /// max(0.25%, 3 x spread), never by its own change, so a steady +2% is
    /// slower; only a calibration counts the A/A change in the floor.
    #[test]
    fn a_workload_with_no_floor_is_judged_by_its_spread() {
        let p = paired([&[1000; 5], &[0; 5]], [&[1020; 5], &[0; 5]]);
        let (floor, calibrated) = floor_of(&p, None, false);
        assert_eq!((floor, calibrated), (FLOOR_MIN, false));
        assert_eq!(verdict(&p, floor, calibrated), Verdict::Slower);
        assert_eq!(floor_of(&p, Some(0.03), false), (0.03, true));
        assert!((floor_of(&p, None, true).0 - 0.02).abs() < 1e-12);
    }

    /// A run that counts no more than its baseline is no count: the
    /// workload is broken, never a change.
    #[test]
    fn a_count_at_its_baseline_is_broken() {
        let p = paired([&[100; 3], &[100; 3]], [&[200; 3], &[100; 3]]);
        assert_eq!(verdict(&p, FLOOR_MIN, false), Verdict::Broken);
        let q = paired([&[1000; 3], &[100; 3]], [&[90, 1000, 1000], &[100; 3]]);
        assert_eq!(verdict(&q, FLOOR_MIN, false), Verdict::Broken);
    }

    #[test]
    fn a_change_inside_its_floor_is_inconclusive() {
        let p = paired([&[1000; 5], &[0; 5]], [&[1002; 5], &[0; 5]]);
        assert_eq!(verdict(&p, 0.0025, true), Verdict::Inconclusive);
        assert_eq!(verdict(&p, 0.001, true), Verdict::Slower);
        let q = paired([&[1000; 5], &[0; 5]], [&[990; 5], &[0; 5]]);
        assert_eq!(verdict(&q, 0.0025, true), Verdict::Faster);
        assert_eq!(verdict(&q, 0.011, true), Verdict::Broken);
        // Rounds of +0%, +2%, -2%: spread 2%, past two thirds of a 0.3% floor.
        let noisy = paired(
            [&[1000, 1000, 1000], &[0; 3]],
            [&[1000, 1020, 980], &[0; 3]],
        );
        assert_eq!(verdict(&noisy, 0.003, true), Verdict::Noisy);
    }

    #[test]
    fn recordings_of_one_program_count_once() {
        assert_eq!(super::program("vt/corpus:nvim-diff"), Some("nvim"));
        assert_eq!(super::program("vt/corpus:pico"), Some("pico"));
        assert_eq!(super::program("vt/ascii"), None);
        // 15 nvim recordings slower count as one program; with 2 others
        // slower and 1 faster that is 3 of 4.
        let mut changes: Vec<(String, f64)> = (0..15)
            .map(|i| (format!("vt/corpus:nvim-{i}"), 0.01))
            .collect();
        changes.push(("vt/corpus:vim".into(), 0.01));
        changes.push(("vt/corpus:man-wide".into(), 0.01));
        changes.push(("vt/corpus:less".into(), -0.01));
        changes.push(("vt/corpus:zsh".into(), 0.0001));
        let refs: Vec<(&str, f64)> = changes.iter().map(|(n, c)| (n.as_str(), *c)).collect();
        let s = signs(&refs);
        assert_eq!((s.programs, s.slower, s.faster), (5, 3, 1));
        assert!((s.p_slower - 5.0 / 16.0).abs() < 1e-12, "{s:?}");
    }

    #[test]
    fn the_sign_test_is_binomial() {
        assert!((tail(4, 4) - 1.0 / 16.0).abs() < 1e-12);
        assert!((tail(4, 0) - 1.0).abs() < 1e-12);
        assert!((tail(10, 9) - 11.0 / 1024.0).abs() < 1e-12);
        assert!(tail(0, 0) >= 1.0);
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
