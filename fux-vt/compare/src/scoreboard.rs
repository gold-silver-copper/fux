//! `scoreboard DIR`: a report of the results the checks left in DIR (each
//! `NAME.json`, as `run.sh quick`, `full`, `deep`, `fuzz` and compare.yml
//! write them), gathered into `DIR/scoreboard.json` and rendered as
//! `DIR/scoreboard.md`. An axis whose results are missing says so; nothing
//! is run here, and nothing is judged: the checks themselves fail.
//!
//! Each ranked row sets fux-vt beside the best other engine that measured
//! it, with the row's own floor ([`Floor`]): ahead or behind only past it,
//! a tie within it, and unranked where no floor was measured.

use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::Path;

/// One row of the summary: the axis, what is measured, fux's figure, and
/// the others' beside it.
struct Row {
    axis: &'static str,
    measure: String,
    fux: String,
    beside: String,
}

/// The results of `check` (`CHECK.json`), if it ran.
fn load(dir: &Path, check: &str) -> Option<Value> {
    let text = std::fs::read_to_string(dir.join(format!("{check}.json"))).ok()?;
    serde_json::from_str(&text).ok()
}

fn number(value: &Value, pointer: &str) -> Option<f64> {
    value.pointer(pointer).and_then(Value::as_f64)
}

fn count(value: &Value, pointer: &str) -> Option<u64> {
    value.pointer(pointer).and_then(Value::as_u64)
}

fn percent(part: u64, whole: u64) -> String {
    if whole == 0 {
        return "–".into();
    }
    let share = (part as f64) / (whole as f64) * 100.0;
    format!("{share:.1}% ({part}/{whole})")
}

const MISSING: &str = "not run";

fn conformance(dir: &Path, rows: &mut Vec<Row>) {
    let Some(esctest) = load(dir, "esctest") else {
        rows.push(row("Conformance", "esctest2 pass rate", MISSING, ""));
        return;
    };
    let rate = |results: &Value, place: &str| {
        let (passed, run) = passed(results, &format!("/{place}"))?;
        Some(percent(passed, run))
    };
    let fux = rate(&esctest, "direct").unwrap_or_else(|| MISSING.into());
    // `deep`'s run in a fux pane has a file of its own, which `full`'s
    // direct run leaves alone.
    let beside = load(dir, "esctest-in-fux")
        .and_then(|in_fux| rate(&in_fux, "in_fux"))
        .map_or_else(String::new, |r| format!("in a fux pane: {r}"));
    rows.push(row(
        "Conformance",
        "esctest2 pass rate, fux-vt",
        &fux,
        &beside,
    ));
}

fn real_programs(dir: &Path, rows: &mut Vec<Row>) {
    let Some(corpus) = load(dir, "corpus") else {
        rows.push(row(
            "Real programs",
            "corpus recordings agreeing",
            MISSING,
            "",
        ));
        return;
    };
    let fux = match (count(&corpus, "/agree"), count(&corpus, "/recordings")) {
        (Some(agree), Some(all)) => percent(agree, all),
        _ => MISSING.into(),
    };
    let engines = corpus
        .get("engines")
        .and_then(Value::as_array)
        .map(|e| {
            e.iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_default();
    rows.push(row(
        "Real programs",
        "corpus recordings agreeing, field by field",
        &fux,
        &format!("judged by {engines}"),
    ));
}

fn multiplexer(dir: &Path, rows: &mut Vec<Row>) {
    let fux = load(dir, "transparency").map_or_else(
        || MISSING.into(),
        |t| {
            let results = t.get("results").and_then(Value::as_array);
            let all = results.map_or(0, Vec::len);
            let flag = |key: &str| {
                results.map_or(0, |r| {
                    r.iter()
                        .filter(|e| e.get(key).is_some_and(|v| !v.is_null() && v != false))
                        .count()
                })
            };
            let same = flag("identical");
            let recorded = flag("recorded");
            format!(
                "{} identical, {recorded} differ as recorded",
                percent(same as u64, all as u64)
            )
        },
    );
    let beside = load(dir, "multiplexers")
        .and_then(|m| m.get("results").and_then(Value::as_array).cloned())
        .map(|results| {
            results
                .iter()
                .filter(|r| r.get("multiplexer").and_then(Value::as_str) != Some("fux"))
                .map(|r| {
                    let name = r.get("multiplexer").and_then(Value::as_str).unwrap_or("?");
                    match (count(r, "/recordings_identical"), count(r, "/recordings")) {
                        (Some(same), Some(all)) => format!("{name} {}", percent(same, all)),
                        _ if r.get("error").is_some() => format!("{name} failed to run"),
                        _ => format!("{name} skipped"),
                    }
                })
                .collect::<Vec<_>>()
                .join("; ")
        })
        .unwrap_or_else(|| "tmux, zellij and herdr: run.sh deep".into());
    rows.push(row(
        "Multiplexer",
        "recordings identical through it (transparency)",
        &fux,
        &beside,
    ));
}

fn speed(dir: &Path, rows: &mut Vec<Row>) {
    let against = load(dir, "against").map_or_else(
        || MISSING.into(),
        |a| {
            let workloads = a.get("workloads").and_then(Value::as_array);
            let mut changes: Vec<f64> = workloads
                .into_iter()
                .flatten()
                .filter_map(|w| number(w, "/change_percent"))
                .collect();
            changes.sort_by(f64::total_cmp);
            let median = changes
                .get(changes.len().checked_div(2).unwrap_or(0))
                .copied()
                .unwrap_or(f64::NAN);
            let size = |key: &str| a.get(key).and_then(Value::as_array).map_or(0, Vec::len);
            format!(
                "{} workloads against {}: median {median:+.2}%, {} flagged, {} too noisy to judge",
                changes.len(),
                a.get("ref").and_then(Value::as_str).unwrap_or("?"),
                size("flagged"),
                size("noisy"),
            )
        },
    );
    rows.push(row("Speed", "instructions per workload", &against, ""));
    let Some(info) = load(dir, "info") else {
        rows.push(row("Speed", "MB/s, fux-vt", MISSING, ""));
        return;
    };
    let ranked = mb_per_s(dir, Some(&info));
    let first = (ranked.iter())
        .filter(|r| r.standing == Standing::Ahead)
        .count();
    let mbs = |engine: &str| {
        let at = format!("/engines_mb_per_s/equal-budget, in process/corpus/{engine}");
        number(&info, &at).map_or_else(|| "?".into(), |v| format!("{v:.0}"))
    };
    rows.push(row(
        "Speed",
        "MB/s on the corpus, equal budget, in process",
        &format!(
            "{} (first on {first} of {} workloads; floor {MBS_FLOOR})",
            mbs("fux-vt"),
            ranked.len()
        ),
        &format!("Ghostty {}, alacritty {}", mbs("ghostty"), mbs("alacritty")),
    ));
}

/// What `feel` measured of `mux`, as `what` renders it, for each of fux,
/// tmux, zellij and herdr.
fn per_mux(feel: &Value, what: &dyn Fn(&Value, &str) -> Option<String>) -> (String, String) {
    let fux = what(feel, "fux").unwrap_or_else(|| MISSING.into());
    let beside = ["tmux", "zellij", "herdr", "direct"]
        .iter()
        .filter_map(|m| what(feel, m).map(|v| format!("{m} {v}")))
        .collect::<Vec<_>>()
        .join("; ");
    (fux, beside)
}

fn feel(dir: &Path, rows: &mut Vec<Row>) {
    let Some(feel) = load(dir, "feel") else {
        for (axis, measure) in [
            ("Latency", "keystroke p50 / p99"),
            ("Footprint", "per pane, idle CPU, bytes per frame"),
        ] {
            rows.push(row(axis, measure, MISSING, ""));
        }
        return;
    };
    for setup in ["idle", "flood"] {
        let (fux, beside) = per_mux(&feel, &|f, m| {
            let p50 = number(f, &format!("/latency/{m}/{setup}/p50_ms"))?;
            let p99 = number(f, &format!("/latency/{m}/{setup}/p99_ms"))?;
            let keys = |k: &str| count(f, &format!("/latency/{m}/{setup}/{k}")).unwrap_or(0);
            Some(format!(
                "{p50:.2} / {p99:.2} ms ({} of {} keys, {} timed out)",
                keys("completed"),
                keys("attempted"),
                keys("timed_out")
            ))
        });
        rows.push(row(
            "Latency",
            &format!("keystroke p50 / p99, {setup}"),
            &fux,
            &beside,
        ));
    }
    // Every workload, corpus and synthetic, from the asking key to the
    // final screen at the client: the sum of the seconds each took.
    let (fux, beside) = per_mux(&feel, &|f, m| {
        let loads = f.pointer(&format!("/throughput/{m}"))?.as_object()?;
        let total: f64 = loads.values().filter_map(|l| number(l, "/seconds")).sum();
        (!loads.is_empty()).then(|| format!("{total:.1} s"))
    });
    rows.push(row(
        "Throughput",
        "seconds to the final screen, every workload",
        &fux,
        &beside,
    ));
    let footprint = [
        (
            "MiB per pane with full history",
            "kib_per_pane_with_full_history",
            1.0 / 1024.0,
            "MiB",
        ),
        (
            "server MiB with 50 panes",
            "server_kib_with_50_panes",
            1.0 / 1024.0,
            "MiB",
        ),
        (
            "idle CPU over 10 s",
            "idle_cpu_seconds_in_10s",
            1000.0,
            "ms",
        ),
    ];
    for (measure, key, scale, unit) in footprint {
        let (fux, beside) = per_mux(&feel, &|f, m| {
            let v = number(f, &format!("/footprint/{m}/{key}"))?;
            Some(format!("{:.1} {unit}", v * scale))
        });
        rows.push(row("Footprint", measure, &fux, &beside));
    }
    let (fux, beside) = per_mux(&feel, &|f, m| {
        let loads = f.pointer(&format!("/throughput/{m}"))?.as_object()?;
        let mut each: Vec<f64> = loads
            .values()
            .filter_map(|l| number(l, "/client_bytes_per_paint"))
            .collect();
        each.sort_by(f64::total_cmp);
        let median = each.get(each.len().checked_div(2)?)?;
        Some(format!("{median:.0} B"))
    });
    rows.push(row(
        "Footprint",
        "bytes to the client per frame (median load)",
        &fux,
        &beside,
    ));
}

/// `fuzz.jsonl`: a line per target run by `run.sh fuzz`.
fn robustness(dir: &Path, rows: &mut Vec<Row>) {
    let Ok(text) = std::fs::read_to_string(dir.join("fuzz.jsonl")) else {
        rows.push(row(
            "Robustness",
            "fuzz time since the last crash",
            MISSING,
            "",
        ));
        return;
    };
    let runs: Vec<Value> = text
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();
    let crashed = |r: &Value| r.get("crashed").and_then(Value::as_bool) == Some(true);
    let since: Vec<&Value> = runs.iter().rev().take_while(|r| !crashed(r)).collect();
    let seconds: f64 = since.iter().filter_map(|r| number(r, "/seconds")).sum();
    let last = runs.iter().rev().find(|r| crashed(r)).map_or_else(
        || "no crash recorded".into(),
        |r| {
            format!(
                "last crash: {} at {}",
                r.get("target").and_then(Value::as_str).unwrap_or("?"),
                r.get("date").and_then(Value::as_str).unwrap_or("?"),
            )
        },
    );
    rows.push(row(
        "Robustness",
        "fuzz time since the last crash",
        &format!("{:.0} min over {} target runs", seconds / 60.0, since.len()),
        &last,
    ));
}

fn row(axis: &'static str, measure: &str, fux: &str, beside: &str) -> Row {
    Row {
        axis,
        measure: measure.into(),
        fux: fux.into(),
        beside: beside.into(),
    }
}

/// A row's floor: how far apart two figures must be before one is ahead.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Floor {
    /// A count, the same on every run: any difference.
    Exact,
    /// A share of the best other figure, measured (A/A, or the spread of
    /// the row's own repeats).
    Percent(f64),
    /// An amount: the measure's resolution (a page of memory, over what it
    /// is divided by).
    Absolute(f64),
    /// None measured: the row is not ranked.
    Unknown(&'static str),
}

impl Floor {
    fn text(self) -> String {
        match self {
            Floor::Exact => "exact".into(),
            Floor::Percent(p) => format!("{p:.2}%"),
            Floor::Absolute(a) => format!("{a:.1}"),
            Floor::Unknown(why) => format!("unknown: {why}"),
        }
    }
}

/// Where fux-vt stands beside the best other engine on a row.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Standing {
    /// Better by more than the floor.
    Ahead,
    /// Within the floor.
    Tie,
    /// Worse by more than the floor.
    Behind,
    /// Not ranked, and why: no floor, no other engine, or another
    /// denominator.
    Unranked(String),
}

impl Standing {
    fn word(&self) -> String {
        match self {
            Self::Ahead => "ahead".into(),
            Self::Tie => "tie".into(),
            Self::Behind => "behind".into(),
            Self::Unranked(why) => format!("unranked: {why}"),
        }
    }

    /// fux-vt's figure beside the best other's, with `floor`.
    fn of(fux_vt: f64, best: f64, lower_is_better: bool, floor: Floor) -> Self {
        let gap = if lower_is_better {
            best - fux_vt
        } else {
            fux_vt - best
        };
        let allowed = match floor {
            Floor::Exact => 0.0,
            Floor::Percent(p) => best.abs() * p / 100.0,
            Floor::Absolute(a) => a,
            Floor::Unknown(why) => return Self::Unranked(why.into()),
        };
        if gap.abs() <= allowed {
            Self::Tie
        } else if gap > 0.0 {
            Self::Ahead
        } else {
            Self::Behind
        }
    }
}

/// A ranked row: fux-vt's figure, the best other engine's, the floor, and
/// where fux-vt stands.
struct Ranked {
    measure: String,
    fux_vt: String,
    best: String,
    floor: Floor,
    standing: Standing,
}

/// The row `measure` from each engine's figure in `figures`: fux-vt beside
/// the best of the others, `shown` as text; `None` if fux-vt has none.
fn rank(
    measure: String,
    figures: &[(String, f64)],
    lower_is_better: bool,
    floor: Floor,
    shown: &dyn Fn(f64) -> String,
) -> Option<Ranked> {
    let fux_vt = figures.iter().find(|(e, _)| e == "fux-vt")?.1;
    let better = |a: f64, b: f64| if lower_is_better { a < b } else { a > b };
    let best = figures
        .iter()
        .filter(|(e, v)| e != "fux-vt" && v.is_finite())
        .fold(None::<&(String, f64)>, |best, x| match best {
            Some(b) if !better(x.1, b.1) => Some(b),
            _ => Some(x),
        });
    let (best, standing) = match best {
        Some((engine, value)) => (
            format!("{engine} {}", shown(*value)),
            Standing::of(fux_vt, *value, lower_is_better, floor),
        ),
        None => (
            "–".into(),
            Standing::Unranked("no other engine measured it".into()),
        ),
    };
    Some(Ranked {
        measure,
        fux_vt: shown(fux_vt),
        best,
        floor,
        standing,
    })
}

/// A pass count and its denominator, from esctest's JSON for a run.
fn passed(results: &Value, pointer: &str) -> Option<(u64, u64)> {
    let passed = count(results, &format!("{pointer}/passed"))?;
    let run = count(results, &format!("{pointer}/run"))?;
    Some((passed, run))
}

/// The least floor a share is ranked past: the speed method's
/// (speed-method.md), however steady a run's repeats.
const FLOOR_MIN_PERCENT: f64 = 0.25;

/// One run's MB/s, by table (`RETENTION, in process`), workload and
/// engine.
type Mbs = BTreeMap<String, BTreeMap<String, BTreeMap<String, f64>>>;

/// The MB/s of each run there is: `fux-bench info`'s (by table, workload
/// and engine), and each `bench-mbs-*` check's (`fux-vt-compare bench
/// --json`, an engine beside fux-vt), of checks that ran and passed. A
/// measure whose contract failed is left out.
fn mbs_runs(dir: &Path, info: Option<&Value>) -> Vec<Mbs> {
    let mut runs = Vec::new();
    if let Some(info) = info {
        let mut run = Mbs::new();
        for (table, workloads) in (info.get("engines_mb_per_s").and_then(Value::as_object))
            .into_iter()
            .flatten()
        {
            for (name, engines) in workloads.as_object().into_iter().flatten() {
                for (engine, v) in engines.as_object().into_iter().flatten() {
                    if let Some(v) = v.as_f64() {
                        let row = run.entry(table.clone()).or_default().entry(name.clone());
                        row.or_default().insert(engine.clone(), v);
                    }
                }
            }
        }
        runs.push(run);
    }
    let mut checks: Vec<String> = (std::fs::read_dir(dir).into_iter().flatten().flatten())
        .filter_map(|e| e.file_name().into_string().ok())
        .filter_map(|n| n.strip_suffix(".json").map(str::to_owned))
        .filter(|n| n.starts_with("bench-mbs-"))
        .collect();
    checks.sort();
    for doc in checks.iter().filter_map(|check| load(dir, check)) {
        let mut run = Mbs::new();
        for table in (doc.get("tables").and_then(Value::as_array))
            .into_iter()
            .flatten()
        {
            let retention = table
                .get("retention")
                .and_then(Value::as_str)
                .unwrap_or("?");
            let place = match table.get("in_process").and_then(Value::as_bool) {
                Some(false) => "own process",
                _ => "in process",
            };
            for w in (table.get("workloads").and_then(Value::as_array))
                .into_iter()
                .flatten()
            {
                let name = w.get("name").and_then(Value::as_str).unwrap_or("?");
                for (engine, cell) in (w.get("engines").and_then(Value::as_object))
                    .into_iter()
                    .flatten()
                {
                    let failed = cell.get("contract_failed").and_then(Value::as_bool) == Some(true);
                    if let (Some(v), false) = (number(cell, "/mb_per_s"), failed) {
                        let row = run.entry(format!("{retention}, {place}")).or_default();
                        row.entry(name.to_owned())
                            .or_default()
                            .insert(engine.clone(), v);
                    }
                }
            }
        }
        runs.push(run);
    }
    runs
}

/// The median of `values`, which are not empty.
fn median(values: &mut [f64]) -> Option<f64> {
    values.sort_by(f64::total_cmp);
    let mid = values.len() / 2;
    match values.len() % 2 {
        1 => values.get(mid).copied(),
        _ => Some((values.get(mid.checked_sub(1)?)? + values.get(mid)?) / 2.0),
    }
}

/// How MB/s is ranked.
const MBS_FLOOR: &str =
    "per workload: max(0.25%, the widest deviation of fux-vt's runs from their median)";

/// The MB/s rows of every run there is (`mbs_runs`): higher is better;
/// each engine's figure the median of its runs, fux-vt's of all its runs
/// beside every engine, and the floor max(0.25%, the widest deviation of
/// fux-vt's runs from their median).
fn mb_per_s(dir: &Path, info: Option<&Value>) -> Vec<Ranked> {
    let runs = mbs_runs(dir, info);
    let mut by: BTreeMap<(&str, &str), BTreeMap<&str, Vec<f64>>> = BTreeMap::new();
    for run in &runs {
        for (table, workloads) in run {
            for (name, engines) in workloads {
                for (engine, v) in engines {
                    let row = by.entry((table.as_str(), name.as_str())).or_default();
                    row.entry(engine.as_str()).or_default().push(*v);
                }
            }
        }
    }
    by.into_iter()
        .filter_map(|((table, name), engines)| {
            let fux = engines.get("fux-vt")?.clone();
            let mut sorted = fux.clone();
            let middle = median(&mut sorted)?;
            let widest = (fux.iter())
                .map(|v| (v / middle - 1.0).abs() * 100.0)
                .fold(FLOOR_MIN_PERCENT, f64::max);
            let figures: Vec<(String, f64)> = engines
                .into_iter()
                .filter_map(|(e, mut v)| Some((e.to_owned(), median(&mut v)?)))
                .collect();
            rank(
                format!("MB/s ({table}): {name}"),
                &figures,
                false,
                Floor::Percent(widest),
                &|v| format!("{v:.1}"),
            )
        })
        .collect()
}

/// esctest's engines: each run's JSON, and where in it each terminal is.
const ESCTEST_RUNS: &[(&str, &str, &str)] = &[
    ("esctest", "/direct", "fux-vt"),
    ("esctest-ghostty", "/direct", "ghostty"),
    ("esctest-alacritty", "/direct", "alacritty"),
    ("esctest-libvterm", "/direct", "libvterm"),
    ("esctest-avt", "/direct", "avt"),
    ("esctest-wezterm", "/direct", "wezterm"),
    ("esctest-vt100", "/direct", "vt100"),
];

/// Every row the last runs ranked fux-vt on beside the other engines:
/// conformance, real programs, instructions per byte, memory, MB/s.
fn ranked(dir: &Path) -> Vec<Ranked> {
    let mut out = Vec::new();
    // esctest: every engine library on one denominator; xterm, the
    // reference, is not one of them.
    let runs: Vec<(&str, (u64, u64))> = ESCTEST_RUNS
        .iter()
        .filter_map(|(check, pointer, engine)| {
            Some((*engine, passed(&load(dir, check)?, pointer)?))
        })
        .collect();
    if let Some(&(_, (_, run))) = runs.iter().find(|(e, _)| *e == "fux-vt") {
        let figures: Vec<(String, f64)> = runs
            .iter()
            .map(|(e, (p, _))| ((*e).to_owned(), *p as f64))
            .collect();
        let one = runs.iter().all(|(_, (_, r))| *r == run);
        let measure = format!("esctest2 tests passed, of {run}");
        let shown = |v: f64| format!("{v:.0}");
        if let Some(mut row) = rank(measure, &figures, false, Floor::Exact, &shown) {
            if !one {
                row.standing = Standing::Unranked("the engines' denominators differ".into());
            }
            out.push(row);
        }
    }
    if let Some(corpus) = load(dir, "corpus-ghostty") {
        let pairs = [
            (
                "corpus recordings agreeing with xterm",
                "/fux-vt/agree",
                "/agree",
                "/recordings",
            ),
            (
                "corpus points agreeing with xterm",
                "/fux-vt/points_agree",
                "/points_agree",
                "/points",
            ),
        ];
        for (measure, fux_vt, ghostty, all) in pairs {
            if let (Some(a), Some(b), Some(all)) = (
                count(&corpus, fux_vt),
                count(&corpus, ghostty),
                count(&corpus, all),
            ) {
                let figures = [
                    ("fux-vt".to_owned(), a as f64),
                    ("ghostty".to_owned(), b as f64),
                ];
                let shown = |n: f64| format!("{n:.0} of {all}");
                out.extend(rank(measure.into(), &figures, false, Floor::Exact, &shown));
            }
        }
    }
    if let Some(instructions) = load(dir, "instructions") {
        for workload in instructions
            .get("workloads")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            // A workload has a row for each retention: each is named so.
            let name = workload.get("name").and_then(Value::as_str).unwrap_or("?");
            let name = match workload.get("retention").and_then(Value::as_str) {
                Some(retention) => format!("{name} ({retention})"),
                None => name.to_owned(),
            };
            let engines = workload.get("engines").and_then(Value::as_object);
            let figures: Vec<(String, f64)> = engines
                .into_iter()
                .flatten()
                .filter_map(|(e, v)| Some((e.clone(), number(v, "/per_byte")?)))
                .collect();
            // The spread of fux-vt's repeats, and of the best other's, the
            // speed method's least floor at least.
            let noise = |e: &str| {
                engines
                    .and_then(|m| m.get(e))
                    .and_then(|v| number(v, "/noise_percent"))
            };
            let Some(row) = rank(
                format!("instructions per byte: {name}"),
                &figures,
                true,
                Floor::Unknown(""),
                &|v| format!("{v:.1}"),
            ) else {
                continue;
            };
            let best = row.best.split_once(' ').map_or("", |(e, _)| e).to_owned();
            let floor = match (noise("fux-vt"), noise(&best)) {
                (Some(a), Some(b)) => Floor::Percent(a.max(b).max(FLOOR_MIN_PERCENT)),
                _ => Floor::Unknown("no spread measured"),
            };
            out.extend(rank(row.measure, &figures, true, floor, &|v| {
                format!("{v:.1}")
            }));
        }
    }
    if let Some(footprint) = load(dir, "footprint") {
        out.extend(memory(&footprint));
    }
    out.extend(mb_per_s(dir, load(dir, "info").as_ref()));
    out
}

/// The memory rows, from `footprint`: bytes per history row and per cell,
/// lower is better. The counter reads whole pages, before and after, so
/// the floor is two pages over what the figure is divided by; a figure
/// whose whole is under a page is under resolution, and not ranked.
fn memory(footprint: &Value) -> Vec<Ranked> {
    const PAGE: f64 = 4096.0;
    let measures: Vec<&Value> = footprint
        .get("measures")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .collect();
    let mut names: Vec<&str> = measures
        .iter()
        .filter_map(|m| m.get("measure").and_then(Value::as_str))
        .collect();
    names.sort_unstable();
    names.dedup();
    let mut out = Vec::new();
    for cols in [80, 200] {
        for name in &names {
            let mine = || {
                measures.iter().copied().filter(move |m| {
                    m.get("measure").and_then(Value::as_str) == Some(name)
                        && count(m, "/cols") == Some(cols)
                })
            };
            let history = name.starts_with("history:");
            let figures: Vec<(String, f64)> = mine()
                .filter_map(|m| {
                    let engine = m.get("engine").and_then(Value::as_str)?.to_owned();
                    let figure = if history {
                        number(m, "/per_history_row/footprint")?
                    } else {
                        number(m, "/footprint_per_cell")?
                    };
                    Some((engine, figure))
                })
                .collect();
            let divisor = mine()
                .find(|m| m.get("engine").and_then(Value::as_str) == Some("fux-vt"))
                .and_then(|m| {
                    if history {
                        number(m, "/history_rows")
                    } else {
                        Some(number(m, "/rows")? * number(m, "/cols")?)
                    }
                })
                .filter(|d| *d > 0.0);
            let floor = divisor.map_or(Floor::Unknown("no divisor"), |d| {
                Floor::Absolute(2.0 * PAGE / d)
            });
            let what = if history {
                "bytes per history row"
            } else {
                "bytes per cell, a screen"
            };
            let measure = format!(
                "memory, {what}: {}, {cols} columns",
                name.strip_prefix("history:").unwrap_or(name)
            );
            let Some(mut row) = rank(measure, &figures, true, floor, &|v| format!("{v:.0} B"))
            else {
                continue;
            };
            let whole = |v: f64| divisor.map_or(0.0, |d| (v * d).abs());
            let fux_vt = figures
                .iter()
                .find(|(e, _)| e == "fux-vt")
                .map_or(0.0, |f| f.1);
            if whole(fux_vt) < PAGE {
                row.standing = Standing::Unranked("under resolution (under a page)".into());
            }
            out.push(row);
        }
    }
    out
}

pub fn run(rest: &[String]) -> Result<bool, String> {
    let [dir] = rest else {
        return Err("scoreboard takes the directory the checks left their results in".into());
    };
    let dir = Path::new(dir);
    let mut rows = Vec::new();
    conformance(dir, &mut rows);
    real_programs(dir, &mut rows);
    multiplexer(dir, &mut rows);
    speed(dir, &mut rows);
    feel(dir, &mut rows);
    robustness(dir, &mut rows);
    let ranked = ranked(dir);
    let json = json!({
        "rows": rows.iter().map(|r| json!({
            "axis": r.axis, "measure": r.measure, "fux": r.fux, "beside": r.beside,
        })).collect::<Vec<_>>(),
        "ranked": ranked.iter().map(|r| json!({
            "measure": r.measure, "fux-vt": r.fux_vt, "best_other": r.best,
            "floor": r.floor.text(), "standing": r.standing.word(),
        })).collect::<Vec<_>>(),
    });
    let mut md = String::from(
        "# fux scoreboard\n\n| Axis | Measure | fux / fux-vt | Beside |\n| --- | --- | --- | --- |\n",
    );
    for r in &rows {
        let _ = writeln!(
            md,
            "| {} | {} | {} | {} |",
            r.axis, r.measure, r.fux, r.beside
        );
    }
    if !ranked.is_empty() {
        let of = |s: &Standing| ranked.iter().filter(|r| r.standing == *s).count();
        let unranked = ranked
            .iter()
            .filter(|r| matches!(r.standing, Standing::Unranked(_)))
            .count();
        let _ = write!(
            md,
            "\n## fux-vt beside the best other engine\n\nAhead on {} of {}, tied on {}, behind on {}, unranked on {unranked}.\n\n| Measure | fux-vt | Best other | Floor | |\n| --- | --- | --- | --- | --- |\n",
            of(&Standing::Ahead),
            ranked.len(),
            of(&Standing::Tie),
            of(&Standing::Behind),
        );
        for r in &ranked {
            let _ = writeln!(
                md,
                "| {} | {} | {} | {} | {} |",
                r.measure,
                r.fux_vt,
                r.best,
                r.floor.text(),
                r.standing.word()
            );
        }
    }
    let mut text = serde_json::to_string_pretty(&json).map_err(|e| format!("json: {e}"))?;
    text.push('\n');
    let write = |name: &str, body: &str| {
        let path = dir.join(name);
        std::fs::write(&path, body).map_err(|e| format!("{}: {e}", path.display()))
    };
    write("scoreboard.json", &text)?;
    write("scoreboard.md", &md)?;
    print!("{md}");
    Ok(true)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    fn dir(name: &str) -> Result<std::path::PathBuf, String> {
        let dir = fuxix::file::scratch()
            .map_err(|e| e.to_string())?
            .join(format!("fux-scoreboard-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        Ok(dir)
    }

    /// With nothing run, every axis says so, and the files are written.
    #[test]
    fn every_axis_says_when_it_was_not_run() -> Result<(), String> {
        let dir = dir("empty")?;
        assert!(super::run(&[dir.to_string_lossy().into_owned()])?);
        let md = std::fs::read_to_string(dir.join("scoreboard.md")).map_err(|e| e.to_string())?;
        for axis in [
            "Conformance",
            "Real programs",
            "Multiplexer",
            "Speed",
            "Latency",
            "Footprint",
            "Robustness",
        ] {
            assert!(md.contains(&format!("| {axis} |")), "{axis} missing:\n{md}");
        }
        assert!(md.contains(super::MISSING));
        let _ = std::fs::remove_dir_all(&dir);
        Ok(())
    }

    /// Beside the best other engine: lower is better for costs, higher for
    /// counts; a tie within the floor, ahead or behind past it, unranked
    /// with no floor.
    #[test]
    fn standing_beside_the_best_other() -> Result<(), String> {
        use super::{Floor, Standing};
        assert_eq!(
            Standing::of(10.0, 20.0, true, Floor::Exact),
            Standing::Ahead
        );
        assert_eq!(
            Standing::of(20.0, 10.0, true, Floor::Exact),
            Standing::Behind
        );
        assert_eq!(
            Standing::of(442.0, 442.0, false, Floor::Exact),
            Standing::Tie
        );
        assert_eq!(
            Standing::of(443.0, 442.0, false, Floor::Exact),
            Standing::Ahead
        );
        assert_eq!(
            Standing::of(100.0, 100.2, true, Floor::Percent(0.25)),
            Standing::Tie
        );
        assert_eq!(
            Standing::of(100.0, 101.0, true, Floor::Percent(0.25)),
            Standing::Ahead
        );
        assert_eq!(
            Standing::of(10.0, 12.0, true, Floor::Absolute(4.0)),
            Standing::Tie
        );
        assert!(matches!(
            Standing::of(1.0, 2.0, false, Floor::Unknown("x")),
            Standing::Unranked(_)
        ));
        let figures = [
            ("ghostty".to_owned(), 14.9),
            ("fux-vt".to_owned(), 13.0),
            ("alacritty".to_owned(), 13.1),
        ];
        let row = super::rank("x".into(), &figures, true, Floor::Percent(0.25), &|v| {
            format!("{v:.1}")
        })
        .ok_or("no row")?;
        assert_eq!(row.best, "alacritty 13.1");
        assert_eq!(row.standing, Standing::Ahead);
        Ok(())
    }

    /// Instructions per byte are ranked past a floor of at least 0.25%,
    /// whatever the repeats' own spread (0.01% here): 0.1% apart is a tie.
    /// MB/s is read as fux-bench info writes it, by table, and ranked as
    /// ranked: past max(0.25%, the widest deviation of
    /// fux-vt's runs from their median), here one run's, so 0.25%.
    #[test]
    fn rows_are_ranked_past_the_floors_their_tools_measure() -> Result<(), String> {
        let dir = dir("floors")?;
        let instructions = json!({"workloads": [{"name": "ascii", "retention": "equal-budget", "engines": {
            "fux-vt": {"per_byte": 100.0, "noise_percent": 0.01},
            "ghostty": {"per_byte": 100.1, "noise_percent": 0.01},
        }}, {"name": "ascii", "retention": "equal-retention", "engines": {
            "fux-vt": {"per_byte": 90.0, "noise_percent": 0.01},
            "ghostty": {"per_byte": 100.0, "noise_percent": 0.01},
        }}]});
        let info = json!({"engines_mb_per_s": {"equal-budget, in process": {
            "ascii": {"fux-vt": 210.5, "ghostty": 180.0},
        }}});
        for (check, value) in [("instructions", &instructions), ("info", &info)] {
            std::fs::write(dir.join(format!("{check}.json")), value.to_string())
                .map_err(|e| e.to_string())?;
        }
        let rows = super::ranked(&dir);
        let row = |measure: &str| rows.iter().find(|r| r.measure.contains(measure));
        let per_byte = row("instructions per byte: ascii (equal-budget)").ok_or("no budget row")?;
        assert_eq!(per_byte.standing, super::Standing::Tie);
        let none =
            row("instructions per byte: ascii (equal-retention)").ok_or("no retention row")?;
        assert_eq!(none.standing, super::Standing::Ahead);
        let mbs = row("MB/s").ok_or("no MB/s row")?;
        assert!(mbs.measure.contains("equal-budget, in process") && mbs.measure.contains("ascii"));
        assert_eq!(mbs.standing, super::Standing::Ahead);
        assert_eq!(mbs.floor, super::Floor::Percent(0.25));
        // With each engine's own run beside it, fux-vt's runs of 100 and 110
        // (their median 105) widen the floor to 4.76%: Ghostty's 104 ties.
        for (engine, fux, theirs) in [("ghostty", 100.0, 104.0), ("alacritty", 110.0, 50.0)] {
            let doc = json!({"kind": "fux-vt-compare bench", "tables": [{"retention": "equal-retention",
                "in_process": true, "workloads": [{"name": "unicode", "engines": {
                    "fux-vt": {"mb_per_s": fux}, engine: {"mb_per_s": theirs}}}]}]});
            let check = format!("bench-mbs-{engine}-synth");
            std::fs::write(dir.join(format!("{check}.json")), doc.to_string())
                .map_err(|e| e.to_string())?;
        }
        let rows = super::ranked(&dir);
        let unicode = (rows.iter())
            .find(|r| r.measure == "MB/s (equal-retention, in process): unicode")
            .ok_or("no unicode row")?;
        assert_eq!(
            (unicode.fux_vt.as_str(), unicode.best.as_str()),
            ("105.0", "ghostty 104.0")
        );
        assert!(
            matches!(unicode.floor, super::Floor::Percent(p) if (p - 100.0 * 5.0 / 105.0).abs() < 1e-9)
        );
        assert_eq!(unicode.standing, super::Standing::Tie);
        let _ = std::fs::remove_dir_all(&dir);
        Ok(())
    }

    /// Fuzz time counts from the last crash on.
    #[test]
    fn fuzz_time_counts_from_the_last_crash() -> Result<(), String> {
        let dir = dir("fuzz")?;
        let lines = [
            json!({"target": "a", "seconds": 600, "crashed": false, "date": "d1"}),
            json!({"target": "b", "seconds": 60, "crashed": true, "date": "d2"}),
            json!({"target": "c", "seconds": 120, "crashed": false, "date": "d3"}),
            json!({"target": "d", "seconds": 180, "crashed": false, "date": "d4"}),
        ];
        let text: String = lines.iter().map(|l| format!("{l}\n")).collect();
        std::fs::write(dir.join("fuzz.jsonl"), text).map_err(|e| e.to_string())?;
        let mut rows = Vec::new();
        super::robustness(&dir, &mut rows);
        let row = rows.first().ok_or("no row")?;
        assert_eq!(row.fux, "5 min over 2 target runs");
        assert_eq!(row.beside, "last crash: b at d2");
        let _ = std::fs::remove_dir_all(&dir);
        Ok(())
    }
}
