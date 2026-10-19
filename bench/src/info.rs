//! `info`: speed that is informational, not compared against REF: MB/s
//! for fux-vt beside Ghostty and alacritty, from `fux-vt/compare/run.sh
//! bench` (wall time, the median of 3 rounds taking the engines in turn,
//! in process), run as it is, its output
//! shown, and read from what its `bench --json` writes, the figures it
//! prints its tables from.
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Instant;

/// The engines `compare` times beside fux-vt here.
const ENGINES: &str = "ghostty,alacritty";
/// MiB per workload: as small as keeps the figures steady, for time.
const MB: &str = "4";

/// Runs `command`, its stdout shown as it comes and kept; stderr shown.
fn shown(command: &mut Command) -> Result<String, String> {
    let out = command
        .stdin(Stdio::null())
        .stderr(Stdio::inherit())
        .output()
        .map_err(|e| e.to_string())?;
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    print!("{text}");
    if !out.status.success() {
        return Err(format!("{command:?} failed ({})", out.status));
    }
    Ok(text)
}

/// Each engine's MB/s in `compare`'s `bench --json` document, by table
/// (its retention, and in process or not), workload and engine; a
/// workload's engine whose measure failed has none.
fn engines(doc: &serde_json::Value) -> Result<serde_json::Value, String> {
    let tables = (doc.get("tables").and_then(serde_json::Value::as_array))
        .ok_or("compare's bench JSON has no tables")?;
    let mut out = serde_json::Map::new();
    for table in tables {
        let retention = table.get("retention").and_then(serde_json::Value::as_str);
        let in_process = table.get("in_process").and_then(serde_json::Value::as_bool);
        let (Some(retention), Some(in_process)) = (retention, in_process) else {
            return Err("a table of compare's bench JSON has no retention or place".into());
        };
        let place = if in_process {
            "in process"
        } else {
            "own process"
        };
        let mut rows = serde_json::Map::new();
        for row in (table.get("workloads").and_then(serde_json::Value::as_array))
            .into_iter()
            .flatten()
        {
            let name = row
                .get("name")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default();
            let cells = (row.get("engines").and_then(serde_json::Value::as_object))
                .into_iter()
                .flatten();
            // A measure whose contract failed (rows kept under equal
            // retention) is none.
            let figures: serde_json::Map<String, serde_json::Value> = cells
                .filter(|(_, cell)| {
                    cell.get("contract_failed")
                        .and_then(serde_json::Value::as_bool)
                        != Some(true)
                })
                .filter_map(|(engine, cell)| Some((engine.clone(), cell.get("mb_per_s")?.clone())))
                .collect();
            rows.insert(name.to_owned(), serde_json::Value::Object(figures));
        }
        out.insert(
            format!("{retention}, {place}"),
            serde_json::Value::Object(rows),
        );
    }
    Ok(serde_json::Value::Object(out))
}

pub fn run(json: &Path) -> Result<bool, String> {
    let root = crate::against::root();
    let started = Instant::now();
    let measures =
        std::path::absolute(json.with_extension("compare.json")).map_err(|e| e.to_string())?;
    println!(
        "fux-vt/compare/run.sh bench --engines {ENGINES} --mb {MB} --json {}",
        measures.display()
    );
    let _ = std::fs::remove_file(&measures);
    let engines = shown(
        Command::new(root.join("fux-vt/compare/run.sh"))
            .args(["bench", "--engines", ENGINES, "--mb", MB, "--json"])
            .arg(&measures),
    )
    .and_then(|_| {
        std::fs::read_to_string(&measures).map_err(|e| format!("{}: {e}", measures.display()))
    })
    .and_then(|text| {
        serde_json::from_str(&text).map_err(|e| format!("{}: {e}", measures.display()))
    })
    .and_then(|doc| engines(&doc));
    if let Err(e) = &engines {
        eprintln!("compare's bench: {e}");
    }
    let engines_secs = started.elapsed().as_secs_f64();
    let value = serde_json::json!({
        "kind": "fux-bench info",
        "engines_mb_per_s": engines.as_ref().cloned().unwrap_or_default(),
        "seconds": { "engines": engines_secs },
    });
    crate::save_json(json, &value)?;
    eprintln!("wrote {}", json.display());
    println!("\ncompare's bench took {engines_secs:.0} s");
    Ok(engines.is_ok())
}

#[cfg(test)]
mod tests {
    /// MB/s from what `compare`'s `bench --json` wrote in a real run (the
    /// scoreboard's `bench-mbs-wezterm-synth-8` at checkpoint 0): each
    /// table's, by workload and engine.
    #[test]
    fn mb_per_s_is_read_from_compare_s_json() -> Result<(), String> {
        let doc: serde_json::Value =
            serde_json::from_str(include_str!("info-compare.json")).map_err(|e| e.to_string())?;
        let value = super::engines(&doc)?;
        let at = |table: &str, row: &str, engine: &str| {
            let cell = value
                .get(table)
                .and_then(|t| t.get(row))
                .and_then(|r| r.get(engine));
            cell.and_then(serde_json::Value::as_f64)
        };
        let mbs = at("equal-budget, in process", "sixel:frames", "fux-vt");
        assert!(mbs.is_some_and(|m| (m - 53.943).abs() < 0.001), "{mbs:?}");
        let wezterm = at("equal-retention, in process", "sixel:frames", "wezterm");
        assert!(wezterm.is_some_and(|m| m > 0.0), "{wezterm:?}");
        assert!(super::engines(&serde_json::json!({})).is_err());
        // A measure whose contract failed is none.
        let failed = serde_json::json!({"tables": [{"retention": "equal-retention", "in_process": true,
            "workloads": [{"name": "w", "engines": {"x": {"mb_per_s": 9.0, "contract_failed": true}}}]}]});
        let none = super::engines(&failed)?;
        assert_eq!(none.pointer("/equal-retention, in process/w/x"), None);
        Ok(())
    }
}
