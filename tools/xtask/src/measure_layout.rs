//! Matched-release legacy resize workload; request latency includes socket/JSON overhead.
use anyhow::{Context, Result, ensure};
use fux_xtask::support::{
    attachment_measure::drain_all,
    local::{Root, completed},
};
use serde_json::{Value, json};
use std::{
    path::Path,
    time::{Duration, Instant},
};

fn sample(binary: &Path, count: usize, operations: usize) -> Result<Value> {
    const DELTA: i32 = 2000;
    let root = Root::new("layout-perf-", &["/bin/cat".into()])?;
    let mut server = root.server(binary)?;
    let result = (|| -> Result<Value> {
        let mut viewers = vec![crate::measure_frames::attach(
            &root.path().join("fux/default.attach.sock"),
            60,
            200,
        )?];
        let mut panes = vec![1u64];
        for index in 0..count - 1 {
            let created = completed(
                &root.control(),
                json!({"command":"split","id":1,
                "target":panes[index / 2],"axis":if index % 2 == 0 {"horizontal"} else {"vertical"},"argv":["/bin/cat"],"final_retain_ms":10000}),
            )?;
            panes.push(created["pane"].as_u64().context("created pane")?);
        }
        drain_all(&mut viewers, Duration::from_millis(100))?;
        let identities = completed(&root.control(), json!({"command":"list","id":1}))?;
        let sizes = |listing: &Value| -> Result<Vec<(Value, Value)>> {
            Ok(listing["workspaces"][0]["tabs"][0]["panes"]
                .as_array()
                .context("pane sizes")?
                .iter()
                .map(|pane| {
                    (
                        pane["geometry"]["width"].clone(),
                        pane["geometry"]["height"].clone(),
                    )
                })
                .collect())
        };
        let original_sizes = sizes(&identities)?;
        let mut previous_sizes = original_sizes.clone();
        // Validate the identical edit sequence before timing it. A request succeeding is
        // insufficient evidence of resize work when ratios round to unchanged cell sizes.
        for index in 0..operations {
            completed(
                &root.control(),
                json!({"command":"resize","id":2,
                "pane":panes[(index / 2) % panes.len()],
                "delta":if index % 2 == 0 {DELTA} else {-DELTA}}),
            )?;
            let listing = completed(&root.control(), json!({"command":"list","id":3}))?;
            let current_sizes = sizes(&listing)?;
            ensure!(
                current_sizes != previous_sizes,
                "resize preflight operation {index} at {count} panes changed no dimensions"
            );
            previous_sizes = current_sizes;
            viewers[0].pump(|_| Ok(()))?;
        }
        ensure!(
            previous_sizes == original_sizes,
            "resize preflight did not restore initial sizes"
        );
        drain_all(&mut viewers, Duration::from_millis(100))?;
        let before_bytes = viewers[0].bytes;
        let pid = server.child.id();
        let before_cpu = crate::measure::cpu(pid)?;
        let start = Instant::now();
        let mut latencies = Vec::with_capacity(operations);
        for index in 0..operations {
            let start = Instant::now();
            completed(
                &root.control(),
                json!({"command":"resize","id":2,
                "pane":panes[(index / 2) % panes.len()],"delta":if index % 2 == 0 {DELTA} else {-DELTA}}),
            )?;
            latencies.push(start.elapsed().as_secs_f64() * 1000.0);
            viewers[0].pump(|_| Ok(()))?;
        }
        let elapsed = start.elapsed().as_secs_f64();
        drain_all(&mut viewers, Duration::from_millis(100))?;
        let cpu = crate::measure::cpu(pid)? - before_cpu;
        let after = completed(&root.control(), json!({"command":"list","id":3}))?;
        ensure!(
            sizes(&after)? == original_sizes,
            "timed resize did not restore sizes"
        );
        let identity = |listing: &Value| -> Result<Vec<(u64, u64)>> {
            listing["workspaces"][0]["tabs"][0]["panes"]
                .as_array()
                .context("panes")?
                .iter()
                .map(|pane| {
                    Ok((
                        pane["id"].as_u64().context("pane id")?,
                        pane["pid"].as_u64().context("pane pid")?,
                    ))
                })
                .collect()
        };
        ensure!(
            identity(&identities)? == identity(&after)?,
            "resize workload changed pane/process identity"
        );
        let request_samples_ms = latencies.clone();
        latencies.sort_by(f64::total_cmp);
        Ok(
            json!({"panes":count,"operations":operations,"delta":DELTA,"preflight_dimension_changes":operations,"elapsed_s":elapsed,"server_cpu_s":cpu,
            "request_samples_ms":request_samples_ms,
            "request_median_ms":latencies[operations / 2],"request_p95_ms":latencies[(operations * 95 / 100).min(operations - 1)],
            "rss_kib":crate::measure::rss(pid)?,"viewer_bytes":viewers[0].bytes - before_bytes}),
        )
    })();
    let cleanup = server.finish();
    let result = result?;
    cleanup?;
    Ok(result)
}

pub fn run(args: Vec<String>) -> Result<()> {
    ensure!(args.len() == 1, "measure-layout BINARY");
    let binary = std::fs::canonicalize(&args[0])?;
    let results = [2, 8, 32]
        .into_iter()
        .map(|panes| sample(&binary, panes, 200))
        .collect::<Result<Vec<_>>>()?;
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({"binary":binary,"geometry":[60,200],
        "measurement":"control request completion, not rendered latency","results":results}))?
    );
    Ok(())
}
