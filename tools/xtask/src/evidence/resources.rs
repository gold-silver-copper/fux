use super::*;
use std::collections::BTreeSet;
fn keys(value: &Value, path: &str) -> Result<BTreeSet<String>> {
    Ok(field(value, path)?
        .as_object()
        .context("components")?
        .keys()
        .cloned()
        .collect())
}
pub fn validate(row: &Value) -> Result<()> {
    let panes = integer(row, "/panes")?;
    ensure!([1, 4, 8].contains(&panes), "pane count");
    let expected: BTreeSet<String> = if field(row, "/backend")? == "zor" {
        ["fux", "zor"].into_iter().map(str::to_owned).collect()
    } else {
        BTreeSet::from(["herdr".into()])
    };
    ensure!(
        keys(row, "/owners_exit")? == expected
            && field(row, "/owners_exit")?
                .as_object()
                .context("owners")?
                .values()
                .all(|v| v == 0)
            && field(row, "/worker_exit_confirmed")? == true,
        "owner scope/cleanup"
    );
    ensure!(
        array(row, "/initial_captures")?.len() as u64 == panes
            && array(row, "/worker_viewports")?.len() as u64 == panes,
        "capture/viewport scope"
    );
    for size in array(row, "/worker_viewports")? {
        let size = size.as_array().context("size")?;
        ensure!(
            size.len() == 2 && size.iter().all(|d| d.as_f64().is_some_and(|d| d > 0.)),
            "viewport size"
        );
    }
    ensure!(
        integer(row, "/harness_capture_reads")? >= panes
            && number(row, "/burst_all_visible_ms")? > 0.
            && number(row, "/burst_all_visible_ms")? < number(row, "/burst/wall_ms")?,
        "capture read/latency"
    );
    let samples = array(row, "/samples")?;
    ensure!(samples.len() == 4, "sample count");
    for (phase, i, j) in [("idle", 0, 1), ("burst", 2, 3)] {
        let interval = field(row, &format!("/{phase}"))?;
        let (before, after) = (&samples[i], &samples[j]);
        ensure!(
            keys(interval, "/components")? == expected
                && keys(before, "/components")? == expected
                && keys(after, "/components")? == expected,
            "omitted component"
        );
        let wall = number(interval, "/wall_ms")?;
        ensure!(
            (wall - (number(after, "/monotonic")? - number(before, "/monotonic")?) * 1000.).abs()
                < 0.001
                && wall >= if phase == "idle" { 3000. } else { 1000. },
            "wall interval"
        );
        for name in &expected {
            let pointer = format!("/components/{name}");
            let a = field(before, &pointer)?;
            let b = field(after, &pointer)?;
            let measured = field(interval, &pointer)?;
            for (key, minimum) in [
                ("pid", 1),
                ("start_abstime", 0),
                ("timebase_numer", 0),
                ("timebase_denom", 0),
            ] {
                let p = format!("/{key}");
                ensure!(
                    field(a, &p)? == field(b, &p)? && integer(b, &p)? > minimum,
                    "process identity/timebase changed"
                );
            }
            // Preserve integer subtraction before conversion, as in Python's accounting.
            let ticks = i128::from(integer(b, "/user_ticks")?)
                + i128::from(integer(b, "/system_ticks")?)
                - i128::from(integer(a, "/user_ticks")?)
                - i128::from(integer(a, "/system_ticks")?);
            let cpu =
                ticks as f64 * number(b, "/timebase_numer")? / number(b, "/timebase_denom")? / 1e6;
            ensure!(
                cpu >= 0. && (cpu - number(measured, "/cpu_ms")?).abs() < 0.001,
                "CPU units/accounting"
            );
            for key in ["rss_bytes", "footprint_bytes"] {
                let p = format!("/{key}");
                ensure!(
                    field(measured, &p)? == field(b, &p)? && integer(b, &p)? > 0,
                    "memory accounting"
                );
            }
        }
    }
    Ok(())
}
pub fn regressions() -> Result<()> {
    let value = report("resources")?;
    verify_sources(&value, "/provenance/sources")?;
    ensure!(
        field(&value, "/provenance/herdr_build/binary_sha256")?
            == field(&value, "/provenance/binaries/herdr/sha256")?,
        "binary provenance"
    );
    let rows = array(&value, "/results")?;
    let actual = rows
        .iter()
        .map(|r| {
            Ok((
                field(r, "/backend")?.as_str().context("backend")?,
                integer(r, "/panes")?,
                integer(r, "/repetition")?,
            ))
        })
        .collect::<Result<BTreeSet<_>>>()?;
    let expected = ["zor", "herdr"]
        .into_iter()
        .flat_map(|b| {
            [1, 4, 8]
                .into_iter()
                .flat_map(move |p| (1..=3).map(move |r| (b, p, r)))
        })
        .collect();
    ensure!(rows.len() == 18 && actual == expected, "resource matrix");
    let calibration = field(&value, "/calibration")?;
    let ratio = number(calibration, "/ratio")?;
    ensure!(
        (ratio - number(calibration, "/converted_cpu_ns")? / number(calibration, "/clock_cpu_ns")?)
            .abs()
            < 0.000005
            && (ratio - 1.).abs() < 0.05,
        "CPU calibration"
    );
    for row in rows {
        validate(row)?;
    }
    let mut invalid = rows
        .iter()
        .find(|r| r["backend"] == "zor")
        .context("zor row")?
        .clone();
    invalid["idle"]["components"]
        .as_object_mut()
        .context("components")?
        .remove("zor");
    ensure!(validate(&invalid).is_err(), "omitted zor accepted");
    for key in ["cpu_ms", "rss_bytes", "footprint_bytes"] {
        let mut invalid = rows[0].clone();
        let v = &mut invalid["idle"]["components"]["fux"][key];
        *v = if let Some(n) = v.as_u64() {
            json!(n + 1)
        } else {
            json!(v.as_f64().context("metric")? + 1.)
        };
        ensure!(validate(&invalid).is_err(), "changed {key} accepted");
    }
    let mut invalid = rows[0].clone();
    let v = &mut invalid["samples"][1]["components"]["fux"]["start_abstime"];
    *v = (v.as_u64().context("start")? + 1).into();
    ensure!(validate(&invalid).is_err(), "PID reuse accepted");
    let mut invalid = rows[0].clone();
    invalid["worker_exit_confirmed"] = false.into();
    ensure!(validate(&invalid).is_err(), "unproven cleanup accepted");
    println!(
        "PASS resource provenance, full matrix, raw CPU/memory accounting and process identity"
    );
    Ok(())
}
#[cfg(test)]
mod tests {
    #[test]
    fn retained_and_negative_cases() {
        super::regressions().unwrap();
    }
}
