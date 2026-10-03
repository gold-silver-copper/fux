//! `scoreboard DIR`: the numbers of the last runs of `run.sh quick`, `full`,
//! `deep` and `fuzz`, which leave their results in DIR, gathered into
//! `DIR/scoreboard.json` and rendered as `DIR/scoreboard.md`. An axis whose
//! results are missing says so; nothing is run here.

use serde_json::{Value, json};
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

fn load(dir: &Path, name: &str) -> Option<Value> {
    let text = std::fs::read_to_string(dir.join(name)).ok()?;
    serde_json::from_str(&text).ok()
}

/// The stamp `run.sh` leaves beside each check: the commit, when, how long,
/// and its exit status.
fn stamp(dir: &Path, check: &str) -> Option<String> {
    let text = std::fs::read_to_string(dir.join(format!("{check}.stamp"))).ok()?;
    Some(text.trim().to_owned())
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
    let Some(esctest) = load(dir, "esctest.json") else {
        rows.push(row("Conformance", "esctest2 pass rate", MISSING, ""));
        return;
    };
    let rate = |place: &str| {
        let passed = count(&esctest, &format!("/{place}/passed"))?;
        let failed = count(&esctest, &format!("/{place}/failed"))?;
        Some(percent(passed, passed.saturating_add(failed)))
    };
    let fux = rate("direct").unwrap_or_else(|| MISSING.into());
    let beside = rate("in_fux").map_or_else(String::new, |r| format!("in a fux pane: {r}"));
    rows.push(row(
        "Conformance",
        "esctest2 pass rate, fux-vt",
        &fux,
        &beside,
    ));
}

fn real_programs(dir: &Path, rows: &mut Vec<Row>) {
    let Some(corpus) = load(dir, "corpus.json") else {
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
    let fux = load(dir, "transparency.json").map_or_else(
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
    let beside = load(dir, "multiplexers.json")
        .and_then(|m| m.get("results").and_then(Value::as_array).cloned())
        .map(|results| {
            results
                .iter()
                .filter(|r| r.get("multiplexer").and_then(Value::as_str) != Some("fux"))
                .map(|r| {
                    let name = r.get("multiplexer").and_then(Value::as_str).unwrap_or("?");
                    match (count(r, "/recordings_identical"), count(r, "/recordings")) {
                        (Some(same), Some(all)) => format!("{name} {}", percent(same, all)),
                        _ => format!("{name} skipped"),
                    }
                })
                .collect::<Vec<_>>()
                .join("; ")
        })
        .unwrap_or_else(|| "tmux and zellij: run.sh deep".into());
    rows.push(row(
        "Multiplexer",
        "recordings identical through it (transparency)",
        &fux,
        &beside,
    ));
}

fn speed(dir: &Path, rows: &mut Vec<Row>) {
    let against = load(dir, "against.json").map_or_else(
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
    let Some(info) = load(dir, "info.json") else {
        rows.push(row("Speed", "MB/s, fux-vt", MISSING, ""));
        return;
    };
    let workloads = info
        .get("engines_mb_per_s")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let fastest = workloads
        .values()
        .filter(|w| {
            let fux = number(w, "/fux-vt").unwrap_or(0.0);
            ["/ghostty", "/alacritty"]
                .iter()
                .all(|other| number(w, other).unwrap_or(0.0) <= fux)
        })
        .count();
    let mbs = |engine: &str| {
        number(&info, &format!("/engines_mb_per_s/corpus/{engine}"))
            .map_or_else(|| "?".into(), |v| format!("{v:.0}"))
    };
    rows.push(row(
        "Speed",
        "MB/s on the corpus",
        &format!(
            "{} (fastest on {fastest} of {} workloads)",
            mbs("fux-vt"),
            workloads.len()
        ),
        &format!("Ghostty {}, alacritty {}", mbs("ghostty"), mbs("alacritty")),
    ));
}

/// What `feel` measured of `mux`, as `what` renders it, for each of fux,
/// tmux and zellij.
fn per_mux(feel: &Value, what: &dyn Fn(&Value, &str) -> Option<String>) -> (String, String) {
    let fux = what(feel, "fux").unwrap_or_else(|| MISSING.into());
    let beside = ["tmux", "zellij", "direct"]
        .iter()
        .filter_map(|m| what(feel, m).map(|v| format!("{m} {v}")))
        .collect::<Vec<_>>()
        .join("; ");
    (fux, beside)
}

fn feel(dir: &Path, rows: &mut Vec<Row>) {
    let Some(feel) = load(dir, "feel.json") else {
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
            Some(format!("{p50:.2} / {p99:.2} ms"))
        });
        rows.push(row(
            "Latency",
            &format!("keystroke p50 / p99, {setup}"),
            &fux,
            &beside,
        ));
    }
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

/// The checks `run.sh` stamps, in the order the commands run them.
const CHECKS: &[&str] = &[
    "corpus",
    "transparency",
    "random",
    "cases",
    "random-wide",
    "random-fux",
    "esctest",
    "against",
    "verdicts-1",
    "esctest-in-fux",
    "multiplexers",
    "feel",
    "info",
];

pub fn run(rest: &[String]) -> Result<bool, String> {
    let [dir] = rest else {
        return Err("scoreboard takes one directory, where run.sh left its results".into());
    };
    let dir = Path::new(dir);
    let mut rows = Vec::new();
    conformance(dir, &mut rows);
    real_programs(dir, &mut rows);
    multiplexer(dir, &mut rows);
    speed(dir, &mut rows);
    feel(dir, &mut rows);
    robustness(dir, &mut rows);
    let stamps: Vec<(&str, String)> = CHECKS
        .iter()
        .filter_map(|c| stamp(dir, c).map(|s| (*c, s)))
        .collect();
    let json = json!({
        "rows": rows.iter().map(|r| json!({
            "axis": r.axis, "measure": r.measure, "fux": r.fux, "beside": r.beside,
        })).collect::<Vec<_>>(),
        "checks": stamps.iter().map(|(c, s)| json!({"check": c, "stamp": s})).collect::<Vec<_>>(),
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
    md.push_str("\n| Check | Commit, when, how long, exit |\n| --- | --- |\n");
    for (check, s) in &stamps {
        let _ = writeln!(md, "| {check} | {s} |");
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
        let dir =
            std::env::temp_dir().join(format!("fux-scoreboard-{name}-{}", std::process::id()));
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
