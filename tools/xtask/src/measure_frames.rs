//! Main's attachment frame benchmark, preserving its workloads and report fields.
use crate::measure::{cpu, round, rss};
use anyhow::{Context, Result, ensure};
use fux_xtask::support::{
    attachment,
    attachment_measure::{Viewer, drain_all, wait_text},
    local::Root,
};
use serde_json::{Value, json};
use std::{
    path::Path,
    time::{Duration, Instant},
};

const DEFAULTS: &[&str] = &[
    "24x80",
    "60x200",
    "24x80+24x80",
    "24x80+24x80+24x80+24x80",
    "24x80+24x80+24x80+24x80+24x80+24x80+24x80+24x80",
    "60x200+24x80",
];
const BURST: &[u8] =
    b"i=0; while [ $i -lt 20000 ]; do echo line$i; i=$((i+1)); done; printf BURST''DONE\\\\n\n";

fn sizes(config: &str) -> Result<Vec<(u16, u16)>> {
    let sizes: Vec<_> = config
        .split('+')
        .map(|size| {
            let (rows, cols) = size.split_once('x').context("expected ROWSxCOLS")?;
            let (rows, cols) = (rows.parse::<u16>()?, cols.parse::<u16>()?);
            ensure!(rows > 0 && cols > 0, "dimensions must be positive");
            Ok((rows, cols))
        })
        .collect::<Result<_>>()?;
    ensure!(sizes.len() <= 64, "at most 64 viewers per configuration");
    Ok(sizes)
}

pub(crate) fn attach(path: &Path, rows: u16, columns: u16) -> Result<Viewer> {
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut peer = loop {
        match attachment::connect(path) {
            Ok(peer) => break peer,
            Err(error) => {
                let retry = error.downcast_ref::<nix::errno::Errno>().is_some_and(|e| {
                    matches!(
                        e,
                        nix::errno::Errno::ENOENT | nix::errno::Errno::ECONNREFUSED
                    )
                });
                if !retry || Instant::now() >= deadline {
                    return Err(error);
                }
                std::thread::sleep(Duration::from_millis(5));
            }
        }
    };
    attachment::send(
        &mut peer,
        &json!({"type":"hello", "rows":rows, "columns":columns}),
    )?;
    let mut viewer = Viewer::new(peer);
    let mut hello = false;
    while !hello {
        ensure!(Instant::now() < deadline, "benchmark hello deadline");
        viewer.pump(|frame| {
            if !hello {
                ensure!(
                    frame == json!({"hello":{}}),
                    "unexpected benchmark hello: {frame}"
                );
                hello = true;
            }
            Ok(())
        })?;
        if !hello {
            std::thread::sleep(Duration::from_millis(1));
        }
    }
    viewer.bytes = 0;
    Ok(viewer)
}

fn input(viewers: &mut [Viewer], bytes: &[u8]) -> Result<()> {
    attachment::send(
        &mut viewers[0].peer,
        &json!({"type":"input", "bytes":bytes}),
    )
}

fn median(values: &mut [f64]) -> f64 {
    values.sort_by(f64::total_cmp);
    let middle = values.len() / 2;
    if values.len().is_multiple_of(2) {
        (values[middle - 1] + values[middle]) / 2.
    } else {
        values[middle]
    }
}

fn measure(binary: &Path, config: &str, keystrokes: usize) -> Result<Value> {
    let sizes = sizes(config)?;
    let root = Root::new("fux-frames-", &["/bin/sh".into()])?;
    let mut server = root.server(binary)?;
    let result: Result<Value> = (|| {
        let path = root.path().join("fux/default.attach.sock");
        let mut viewers = sizes
            .into_iter()
            .map(|(rows, cols)| attach(&path, rows, cols))
            .collect::<Result<Vec<_>>>()?;
        drain_all(&mut viewers, Duration::from_millis(500))?;
        let pid = server.child.id();
        let cpu_before = cpu(pid)?;
        let mut latencies = Vec::new();
        let mut byte_counts = Vec::new();
        for index in 0..keystrokes {
            if index > 0 && index % 40 == 0 {
                input(&mut viewers, &[0x15])?;
                wait_text(
                    &mut viewers,
                    |text| !text.contains('a'),
                    Duration::from_secs(5),
                )?;
            }
            let wanted = index % 40 + 1;
            let before = viewers[0].bytes;
            let begin = Instant::now();
            input(&mut viewers, b"a")?;
            wait_text(
                &mut viewers,
                |text| text.matches('a').count() >= wanted,
                Duration::from_secs(5),
            )?;
            latencies.push(begin.elapsed().as_secs_f64());
            byte_counts.push((viewers[0].bytes - before) as f64);
        }
        let cpu_keys = cpu(pid)? - cpu_before;
        input(&mut viewers, &[0x15])?;
        drain_all(&mut viewers, Duration::from_millis(300))?;
        let before = viewers[0].bytes;
        let cpu_before = cpu(pid)?;
        input(&mut viewers, BURST)?;
        let begin = Instant::now();
        wait_text(
            &mut viewers,
            |text| text.contains("BURSTDONE"),
            Duration::from_secs(60),
        )?;
        let burst = begin.elapsed().as_secs_f64();
        drain_all(&mut viewers, Duration::from_millis(300))?;
        let cpu_burst = cpu(pid)? - cpu_before;
        let latency_median = median(&mut latencies);
        // Preserve Python's rank-minus-one percentile, including its -1 index for N=1.
        let rank = (keystrokes as f64 * 0.95) as usize;
        let p95 = latencies[if rank == 0 { keystrokes - 1 } else { rank - 1 }];
        let bytes_median = median(&mut byte_counts) as u64;
        Ok(json!({"config":config,
            "keystroke_bytes_median":bytes_median,"keystroke_bytes_max":byte_counts[keystrokes-1] as u64,
            "keystroke_latency_median_ms":round(latency_median*1000.,2),"keystroke_latency_p95_ms":round(p95*1000.,2),
            "server_cpu_s_per_1000_keystrokes":round(cpu_keys*1000./keystrokes as f64,3),
            "burst_s":round(burst,3),"burst_server_cpu_s":round(cpu_burst,3),
            "burst_bytes_first_viewer":viewers[0].bytes-before,"rss_after_kib":rss(pid)?}))
    })();
    let cleanup = server.finish();
    let value = result?;
    cleanup?;
    Ok(value)
}

pub fn run(args: Vec<String>) -> Result<()> {
    let mut args = args.into_iter();
    let binary = std::fs::canonicalize(
        args.next()
            .context("measure-frames BINARY [--keystrokes N] [--config ROWSxCOLS[+...]]")?,
    )?;
    let mut keystrokes = 100;
    let mut configs = Vec::new();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--keystrokes" => keystrokes = args.next().context("missing keystrokes")?.parse()?,
            "--config" => configs.push(args.next().context("missing config")?),
            _ => anyhow::bail!("unknown measure-frames argument: {arg}"),
        }
    }
    ensure!(
        (1..=10000).contains(&keystrokes),
        "keystrokes must be 1..=10000"
    );
    if configs.is_empty() {
        configs = DEFAULTS.iter().map(|s| (*s).into()).collect();
    }
    for config in &configs {
        sizes(config)?;
    }
    let results = configs
        .iter()
        .map(|config| measure(&binary, config, keystrokes))
        .collect::<Result<Vec<_>>>()?;
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({"binary":binary,"results":results}))?
    );
    Ok(())
}
