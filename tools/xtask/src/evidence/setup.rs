use super::*;
use std::collections::BTreeSet;
pub fn validate(value: &Value) -> Result<()> {
    ensure!(
        field(value, "/server_exit")? == 0
            && field(value, "/worker_exit_confirmed")? == true
            && field(value, "/prompt_input_sent")? == false
            && field(value, "/controller_reused")? == true,
        "setup ownership/input"
    );
    let calls = array(value, "/calls")?;
    let cold = calls.first().context("cold diagnostic")?;
    ensure!(
        field(cold, "/purpose")? == "unavailable-service diagnostic"
            && field(cold, "/exit_code")? == 1,
        "cold diagnostic exit"
    );
    let zor = field(value, "/backend")? == "zor";
    let pane = if zor {
        ensure!(
            field(cold, "/stderr")?
                .as_str()
                .context("stderr")?
                .contains("zor service unavailable; start `zor serve` with the same --directory")
                && field(value, "/worker_survives_controller_stop")? == true,
            "zor unavailable diagnostic"
        );
        Value::Null
    } else {
        let error: Value =
            serde_json::from_str(field(cold, "/stderr")?.as_str().context("stderr")?)?;
        ensure!(
            field(&error, "/error/code")? == "server_not_running"
                && field(&error, "/error/message")?
                    .as_str()
                    .context("message")?
                    .contains("run `herdr` to start or attach it")
                && field(value, "/worker_survives_controller_stop")?.is_null(),
            "herdr unavailable diagnostic"
        );
        let created = calls
            .iter()
            .find(|c| c["purpose"] == "create fixture workspace")
            .context("workspace creation")?;
        let stdout: Value =
            serde_json::from_str(field(created, "/stdout")?.as_str().context("stdout")?)?;
        field(&stdout, "/result/root_pane/pane_id")?.clone()
    };
    let populated = |call: &Value| -> Result<bool> {
        if zor {
            if field(call, "/argv")? != &json!(["dashboard", "--once"]) {
                return Ok(false);
            }
            let parsed: Value =
                serde_json::from_str(field(call, "/stdout")?.as_str().context("stdout")?)?;
            Ok(array(&parsed, "/rows")?
                .iter()
                .any(|r| r["kind"] == "observation"))
        } else {
            if field(call, "/argv")? != &json!(["agent", "list"]) || field(call, "/exit_code")? != 0
            {
                return Ok(false);
            }
            let parsed: Value =
                serde_json::from_str(field(call, "/stdout")?.as_str().context("stdout")?)?;
            Ok(array(&parsed, "/result/agents")?
                .iter()
                .any(|r| r["pane_id"] == pane))
        }
    };
    let mut first = None;
    for (index, call) in calls.iter().enumerate() {
        if populated(call)? {
            first = Some(index);
            break;
        }
    }
    ensure!(
        integer(value, "/commands_to_visible_overview")?
            == u64::try_from(
                array(value, "/startup")?.len() + first.context("no populated overview")?
            )?,
        "hidden setup commands"
    );
    ensure!(
        integer(value, "/initial_overview_polls")?
            == calls
                .iter()
                .filter(|c| c["purpose"] == "wait for initial overview")
                .count() as u64,
        "hidden discovery polls"
    );
    let reuse = calls
        .iter()
        .find(|c| c["purpose"] == "reuse controller and show overview")
        .context("reuse call")?;
    ensure!(populated(reuse)?, "reuse empty overview");
    for call in calls {
        ensure!(
            field(call, "/exit_code")?
                == if field(call, "/purpose")?
                    .as_str()
                    .context("purpose")?
                    .contains("diagnostic")
                {
                    1
                } else {
                    0
                },
            "unexpected command exit"
        );
    }
    Ok(())
}
pub fn verify(args: Vec<String>) -> Result<()> {
    match args.as_slice() {
        [] => regressions(),
        [path] => report_checks(&serde_json::from_slice(&fs::read(path)?)?),
        _ => anyhow::bail!("usage: verify-controller-setup [REPORT_JSON]"),
    }
}
fn report_checks(value: &Value) -> Result<()> {
    if value["provenance"]["harness_kind"] == "rust-controller-setup" {
        verify_hash(
            "tools/xtask/src/setup_capture.rs",
            field(value, "/provenance/harness_sha256")?,
        )?;
        verify_hash(
            "tools/xtask/src/setup-worker.c",
            field(value, "/provenance/worker_sha256")?,
        )?;
    } else {
        verify_hash(
            "tools/comparisons/controller_setup.py",
            field(value, "/provenance/harness_sha256")?,
        )?;
        verify_hash(
            "tools/comparisons/prompt_boundary.py",
            field(value, "/provenance/helper_sha256")?,
        )?;
    }
    verify_sources(value, "/provenance/source_hashes")?;
    ensure!(
        field(value, "/provenance/herdr_build/binary_sha256")?
            == field(value, "/provenance/binaries/herdr/sha256")?
            && field(value, "/provenance/herdr_build")? == &report("controller-setup-build")?,
        "build provenance"
    );
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
    ensure!(rows.len() == 6 && actual == expected, "setup matrix");
    for row in rows {
        validate(row)?;
    }
    Ok(())
}
pub fn regressions() -> Result<()> {
    let value = report("controller-setup")?;
    report_checks(&value)?;
    let rows = array(&value, "/results")?;
    let mut invalid = rows
        .iter()
        .find(|r| r["backend"] == "herdr")
        .context("herdr row")?
        .clone();
    for call in invalid["calls"].as_array_mut().context("calls")? {
        if call["argv"] == json!(["agent", "list"]) && call["exit_code"] == 0 {
            call["stdout"] = "{\"result\":{\"agents\":[]}}".into();
        }
    }
    ensure!(validate(&invalid).is_err(), "empty overview accepted");
    for row in rows {
        let mut invalid = row.clone();
        invalid["calls"][0]["exit_code"] = 2.into();
        ensure!(validate(&invalid).is_err(), "syntax error accepted");
        let mut invalid = row.clone();
        invalid["commands_to_visible_overview"] =
            (integer(row, "/commands_to_visible_overview")? - 1).into();
        ensure!(validate(&invalid).is_err(), "hidden poll accepted");
    }
    for (name, replacement) in [
        ("prompt_input_sent", json!(true)),
        ("worker_exit_confirmed", json!(false)),
        ("server_exit", json!(1)),
    ] {
        let mut invalid = rows[0].clone();
        invalid[name] = replacement;
        ensure!(validate(&invalid).is_err(), "invalid {name} accepted");
    }
    println!("PASS setup provenance, command accounting, unavailable diagnostics and cleanup");
    Ok(())
}
#[cfg(test)]
mod tests {
    #[test]
    fn retained_and_negative_cases() {
        super::regressions().unwrap();
    }
}
