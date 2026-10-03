//! `info`: speed that is informational, not compared against REF:
//! - MB/s for fux-vt beside Ghostty and alacritty, from
//!   `fux-vt/compare/run.sh bench` (wall time, best of 3, in process);
//! - fux-diff's `--speed`: fux-vt's parse time beside its last release.
//!
//! Each is run as it is, and its table kept as it printed it, and parsed.
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

/// A table of a header line of column names and rows of a name and
/// numbers, as `compare`'s `bench` prints it: each row's figures by
/// column, named as the header names them unless `named`.
fn table(text: &str, header: usize, named: &[&str]) -> serde_json::Value {
    let mut lines = text.lines().skip(header);
    let mut columns: Vec<&str> = lines
        .next()
        .map(|l| l.split_whitespace().collect())
        .unwrap_or_default();
    if !named.is_empty() {
        columns = named.to_vec();
    }
    let mut rows = serde_json::Map::new();
    for line in lines {
        let mut words = line.split_whitespace();
        let Some(name) = words.next() else {
            break;
        };
        let figures: serde_json::Map<String, serde_json::Value> = columns
            .iter()
            .zip(words)
            .map(|(column, figure)| {
                let value = figure
                    .parse::<f64>()
                    .map_or_else(|_| serde_json::json!(figure), |n| serde_json::json!(n));
                ((*column).to_owned(), value)
            })
            .collect();
        rows.insert(name.to_owned(), serde_json::Value::Object(figures));
    }
    serde_json::Value::Object(rows)
}

pub fn run(json: &Path) -> Result<bool, String> {
    let root = crate::against::root();
    let started = Instant::now();
    println!("fux-vt/compare/run.sh bench --engines {ENGINES} --mb {MB}");
    let engines = shown(Command::new(root.join("fux-vt/compare/run.sh")).args([
        "bench",
        "--engines",
        ENGINES,
        "--mb",
        MB,
    ]));
    let engines_secs = started.elapsed().as_secs_f64();
    let started = Instant::now();
    println!("\nfux-diff --speed");
    let diff = shown(
        Command::new("cargo")
            .args(["run", "--release", "--quiet", "--manifest-path"])
            .arg(root.join("diff/Cargo.toml"))
            .args(["--", "--speed"])
            .current_dir(&root),
    );
    let diff_secs = started.elapsed().as_secs_f64();
    let value = serde_json::json!({
        "kind": "fux-bench info",
        "engines_mb_per_s": engines.as_ref().map(|t| table(t, 1, &[])).unwrap_or_default(),
        "diff_speed": diff
            .as_ref()
            .map(|t| table(t, 1, &["baseline_us", "current_us", "ratio"])).unwrap_or_default(),
        "seconds": { "engines": engines_secs, "diff_speed": diff_secs },
    });
    crate::save_json(json, &value)?;
    eprintln!("wrote {}", json.display());
    println!("\ncompare's bench took {engines_secs:.0} s, fux-diff --speed {diff_secs:.0} s");
    Ok(engines.is_ok() && diff.is_ok())
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_table_is_read_by_its_columns() {
        let text = "MB/s, best of 3\n                           fux-vt    ghostty\nascii   210.5   180.0\ncorpus  error  95.1\n\nascii   lines\n";
        let value = super::table(text, 1, &[]);
        let at = |row: &str, column: &str| value.get(row).and_then(|r| r.get(column)).cloned();
        assert_eq!(at("ascii", "fux-vt"), Some(serde_json::json!(210.5)));
        assert_eq!(at("corpus", "fux-vt"), Some(serde_json::json!("error")));
        assert_eq!(at("corpus", "ghostty"), Some(serde_json::json!(95.1)));
        assert!(value.get("lines").is_none());
        let speed = "fux-vt parse time\nstream baseline µs current µs ratio\nascii 10 9 0.90\n";
        let value = super::table(speed, 1, &["baseline_us", "current_us", "ratio"]);
        let ratio = value.get("ascii").and_then(|r| r.get("ratio")).cloned();
        assert_eq!(ratio, Some(serde_json::json!(0.9)));
    }
}
