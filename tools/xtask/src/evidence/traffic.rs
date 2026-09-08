use super::*;
use std::collections::BTreeSet;
pub fn summarize(records: &[Value], start: f64, end: f64) -> Result<Value> {
    let mut captures = 0;
    let mut lists = 0;
    let mut events = 0;
    let mut request = 0;
    let mut reply = 0;
    let mut text = 0;
    let mut event_bytes = 0;
    for record in records {
        let finished = number(record, "/finished")?;
        if !(start <= finished && finished < end) {
            continue;
        }
        match field(record, "/command")?.as_str().context("command")? {
            "capture" => {
                captures += 1;
                request += integer(record, "/request_bytes")?;
                reply += integer(record, "/reply_bytes")?;
                text += integer(record, "/text_bytes")?;
            }
            "list" => lists += 1,
            "event" => {
                events += 1;
                event_bytes += integer(record, "/bytes")?;
            }
            _ => {}
        }
    }
    Ok(
        json!({"duration_ms":(end-start)*1000.,"captures":captures,"capture_request_bytes":request,"capture_reply_bytes":reply,"capture_text_bytes":text,"lists":lists,"events":events,"event_bytes":event_bytes}),
    )
}
pub fn validate(row: &Value) -> Result<()> {
    let panes = integer(row, "/panes")?;
    ensure!(
        [1, 4, 8].contains(&panes) && field(row, "/worker_count")? == panes,
        "worker scope"
    );
    ensure!(
        field(row, "/owners_exit")? == &json!({"fux":0,"zor":0})
            && field(row, "/worker_exit_confirmed")? == true,
        "owned cleanup"
    );
    let bounds = field(row, "/boundaries")?;
    let (a, b, c, d) = (
        number(bounds, "/idle_start")?,
        number(bounds, "/idle_end")?,
        number(bounds, "/burst_start")?,
        number(bounds, "/burst_end")?,
    );
    ensure!(a < b && b <= c && c < d, "interval boundaries");
    let records = array(row, "/records")?;
    for (phase, start, end) in [("idle", a, b), ("burst", c, d)] {
        ensure!(
            field(row, &format!("/{phase}"))? == &summarize(records, start, end)?,
            "{phase} accounting differs from records: retained={} computed={}",
            field(row, &format!("/{phase}"))?,
            summarize(records, start, end)?
        );
    }
    ensure!(
        number(row, "/idle/duration_ms")? >= 4200. && integer(row, "/idle/captures")? == 0,
        "idle capture traffic"
    );
    ensure!(
        integer(row, "/burst/captures")? >= panes
            && integer(row, "/burst/capture_reply_bytes")?
                >= integer(row, "/burst/capture_text_bytes")?
            && integer(row, "/burst/capture_text_bytes")? > 0,
        "burst accounting"
    );
    ensure!(
        number(row, "/all_burst_captured_ms")? > 0.
            && number(row, "/all_burst_captured_ms")? < number(row, "/burst/duration_ms")?,
        "burst latency"
    );
    for record in records {
        ensure!(
            ["list", "capture", "subscribe", "event"]
                .contains(&field(record, "/command")?.as_str().context("command")?),
            "harness input counted as observer traffic"
        );
    }
    for workspace in 0..panes {
        let mut fresh = false;
        for record in records {
            if field(record, "/workspace")? == workspace
                && record.get("burst_done") == Some(&Value::Bool(true))
            {
                let finished = number(record, "/finished")?;
                fresh |= c <= finished && finished < d;
            }
        }
        ensure!(fresh, "workspace {workspace} has no fresh burst capture");
    }
    Ok(())
}
fn report_checks(value: &Value) -> Result<()> {
    verify_sources(value, "/provenance/sources")?;
    let rows = array(value, "/results")?;
    let actual = rows
        .iter()
        .map(|r| Ok((integer(r, "/panes")?, integer(r, "/repetition")?)))
        .collect::<Result<BTreeSet<_>>>()?;
    let expected = [1, 4, 8]
        .into_iter()
        .flat_map(|p| (1..=3).map(move |r| (p, r)))
        .collect();
    ensure!(rows.len() == 9 && actual == expected, "traffic matrix");
    for row in rows {
        validate(row)?;
    }
    ensure!(
        field(value, "/herdr_internal_reads")?.is_null()
            && field(value, "/herdr_measurement")?
                .as_str()
                .context("measurement")?
                .contains("in-process"),
        "invented herdr traffic"
    );
    Ok(())
}
pub fn verify(args: Vec<String>) -> Result<()> {
    match args.as_slice() {
        [] => regressions(),
        [path] => report_checks(&serde_json::from_slice(&fs::read(path)?)?),
        _ => anyhow::bail!("usage: verify-capture-traffic [REPORT_JSON]"),
    }
}
pub fn regressions() -> Result<()> {
    let value = report("capture-traffic")?;
    report_checks(&value)?;
    let rows = array(&value, "/results")?;
    for name in [
        "captures",
        "capture_request_bytes",
        "capture_reply_bytes",
        "capture_text_bytes",
    ] {
        let mut invalid = rows[0].clone();
        invalid["burst"][name] = 0.into();
        ensure!(validate(&invalid).is_err(), "zero {name} accepted");
    }
    let mut invalid = rows[0].clone();
    invalid["records"]
        .as_array_mut()
        .context("records")?
        .push(json!({"command":"send-keys","finished":0}));
    ensure!(validate(&invalid).is_err(), "input traffic accepted");
    let mut invalid = rows.last().context("last row")?.clone();
    for record in invalid["records"].as_array_mut().context("records")? {
        if record["workspace"] == 0 {
            record["burst_done"] = false.into();
        }
    }
    ensure!(validate(&invalid).is_err(), "stale workspace accepted");
    println!(
        "PASS capture traffic provenance, matrix, byte accounting and fresh workspace coverage"
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
