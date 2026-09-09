use super::*;
use std::collections::BTreeSet;
fn validate(row: &Value) -> Result<()> {
    ensure!(
        field(row, "/expected")? == "blocked" && field(row, "/prompt_input_sent")? == false,
        "blocker/input scope"
    );
    ensure!(
        field(row, "/owners_exit")?
            .as_object()
            .context("owners")?
            .values()
            .all(|v| v == 0)
            && field(row, "/agent_pid_exit_checked")? == true
            && integer(row, "/agent_pid")? > 1,
        "cleanup evidence"
    );
    for path in ["/visible_capture", "/final_capture"] {
        let text = field(row, path)?.to_string();
        ensure!(
            text.contains("Sign in with ChatGPT") && text.contains("Press enter to continue"),
            "wrong blocker screen"
        );
    }
    ensure!(
        number(row, "/observation_window_ms")? >= 3000.
            && number(row, "/close_to_absence_ms")? > 0.,
        "observation window"
    );
    let samples = array(row, "/samples")?;
    ensure!(!samples.is_empty(), "empty samples");
    for pair in samples.windows(2) {
        ensure!(
            number(&pair[0], "/elapsed_ms")? < number(&pair[1], "/elapsed_ms")?,
            "sample timing order"
        );
    }
    let matched = samples
        .iter()
        .filter(|s| s["state"] == "blocked")
        .collect::<Vec<_>>();
    let first = matched
        .first()
        .map(|s| field(s, "/elapsed_ms"))
        .transpose()?
        .unwrap_or(&Value::Null);
    ensure!(
        field(row, "/first_blocked_ms")? == first,
        "censored blocker latency"
    );
    let zor = field(row, "/backend")? == "zor";
    let (entries, state) = if zor {
        ensure!(
            !matched.is_empty()
                && array(row, "/after_close/observations")?.is_empty()
                && integer(row, "/final_capture/input_sequence")? == 0,
            "zor observation closure"
        );
        ("observations", "state")
    } else {
        ensure!(
            matched.is_empty()
                && array(row, "/after_close/agents")?.is_empty()
                && samples.iter().any(|s| s["state"] == "unknown"),
            "herdr unmatched closure"
        );
        ("agents", "agent_status")
    };
    for sample in samples {
        let rows = array(sample, &format!("/observation/{entries}"))?;
        let actual = rows
            .first()
            .map(|r| field(r, &format!("/{state}")))
            .transpose()?
            .unwrap_or(&Value::Null);
        ensure!(
            field(sample, "/state")? == actual,
            "reported state differs from controller reply"
        );
    }
    Ok(())
}
fn report_checks(value: &Value, historical: bool) -> Result<()> {
    if historical {
        retained_sources("detection-freshness", value, "/provenance/sources")?;
    } else {
        verify_sources(value, "/provenance/sources")?;
    }
    let rows = array(value, "/results")?;
    let actual = rows
        .iter()
        .map(|r| {
            Ok((
                field(r, "/backend")?.as_str().context("backend")?,
                integer(r, "/repetition")?,
            ))
        })
        .collect::<Result<BTreeSet<_>>>()?;
    let expected = ["zor", "herdr"]
        .into_iter()
        .flat_map(|b| (1..=3).map(move |r| (b, r)))
        .collect();
    ensure!(rows.len() == 6 && actual == expected, "freshness matrix");
    for row in rows {
        validate(row)?;
    }
    Ok(())
}
pub fn verify(args: Vec<String>) -> Result<()> {
    match args.as_slice() {
        [] => regressions(),
        [path] => report_checks(&serde_json::from_slice(&fs::read(path)?)?, false),
        _ => anyhow::bail!("usage: verify-detection-freshness [REPORT_JSON]"),
    }
}
pub fn regressions() -> Result<()> {
    let value = retained_report("detection-freshness")?;
    report_checks(&value, true)?;
    let rows = array(&value, "/results")?;
    let mut invalid = rows
        .iter()
        .find(|r| r["backend"] == "herdr")
        .context("herdr row")?
        .clone();
    invalid["first_blocked_ms"] = 0.into();
    ensure!(
        validate(&invalid).is_err(),
        "unmatched zero latency accepted"
    );
    for (key, replacement) in [
        ("final_capture", json!({})),
        ("agent_pid_exit_checked", json!(false)),
    ] {
        let mut invalid = rows[0].clone();
        invalid[key] = replacement;
        ensure!(validate(&invalid).is_err(), "invalid {key} accepted");
    }
    let mut invalid = rows[0].clone();
    invalid["samples"]
        .as_array_mut()
        .context("samples")?
        .last_mut()
        .context("sample")?["state"] = "idle".into();
    ensure!(validate(&invalid).is_err(), "invented state accepted");
    println!(
        "PASS freshness provenance, timing, censoring, controller state and pane-loss evidence"
    );
    Ok(())
}
#[cfg(test)]
mod tests {
    #[test]
    fn historical_sources_are_explicit_and_complete() -> anyhow::Result<()> {
        use super::*;
        let mut value = retained_report("detection-freshness")?;
        report_checks(&value, true)?;
        ensure!(
            report_checks(&value, false).is_err(),
            "historical report accepted as current evidence"
        );
        value["provenance"]["sources"]["zor/src/watch.rs"] = json!("0".repeat(64));
        ensure!(
            report_checks(&value, true).is_err(),
            "changed source accepted"
        );
        value["provenance"]["sources"]
            .as_object_mut()
            .context("sources")?
            .remove("zor/src/watch.rs");
        ensure!(
            report_checks(&value, true).is_err(),
            "missing source accepted"
        );
        Ok(())
    }

    #[test]
    fn retained_and_negative_cases() {
        super::regressions().unwrap();
    }
}
