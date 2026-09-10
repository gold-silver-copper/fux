//! Zor's actual workspace IPC through owned transparent proxies.
use anyhow::{Context, Result, ensure};
use fux_xtask::support::{
    local::{Root, completed, until},
    process::{self, Guard, OwnedProcess},
    traffic_proxy::Proxy,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant},
};
fn digest(p: &Path) -> Result<String> {
    Ok(format!("{:x}", Sha256::digest(fs::read(p)?)))
}
fn case(count: usize, binaries: &BTreeMap<&str, PathBuf>, worker: &Path) -> Result<Value> {
    let mut root = Root::new("traffic-rs-", &[worker.to_str().context("worker")?.into()])?;
    fs::create_dir(root.path().join("workers"))?;
    for (k, p) in [("WORKER_DIR", "workers"), ("XDG_CACHE_HOME", "cache")] {
        root.env.insert(
            k.into(),
            root.path().join(p).to_str().context("root")?.into(),
        );
    }
    let log = tempfile::tempfile()?;
    let mut owners: Vec<(&str, Guard)> = Vec::new();
    let mut proxies = Vec::new();
    let mut panes = Vec::new();
    let epoch = Instant::now();
    let cli = |name: &str, args: &[&str]| -> Result<Value> {
        let mut c = root.command(&binaries[name]);
        c.args(args);
        let r = process::output(c, Duration::from_secs(10), 1024 * 1024)?;
        ensure!(
            r.status.success(),
            "{name} {args:?}: {}",
            String::from_utf8_lossy(&r.stderr)
        );
        if r.stdout.is_empty() {
            Ok(Value::Null)
        } else {
            Ok(serde_json::from_slice(&r.stdout)
                .unwrap_or_else(|_| String::from_utf8_lossy(&r.stdout).into_owned().into()))
        }
    };
    let measured = (|| -> Result<Value> {
        let mut start = |name: &'static str, args: &[&str]| -> Result<()> {
            let mut c = root.command(&binaries[name]);
            c.args(args)
                .stdin(Stdio::null())
                .stdout(log.try_clone()?)
                .stderr(log.try_clone()?);
            owners.push((name, Guard(c.spawn()?)));
            Ok(())
        };
        start("fux", &["serve"])?;
        until(Duration::from_secs(8), || {
            Ok(root.control().exists().then_some(()))
        })?;
        for i in 1..count {
            cli("fux", &["workspace", "new", &format!("pane{i}")])?;
        }
        for i in 0..count {
            let name = if i == 0 {
                "default".into()
            } else {
                format!("pane{i}")
            };
            let front = root.path().join("fux").join(format!("{name}.sock"));
            let upstream = root.path().join(format!("up{i}.sock"));
            proxies.push(Proxy::start(&front, &upstream, epoch)?);
            let listing = completed(&upstream, json!({"id":1,"command":"list"}))?;
            let pane = listing
                .pointer("/workspaces/0/tabs/0/panes/0/id")
                .context("pane")?
                .clone();
            panes.push((upstream, pane));
        }
        start("zor", &["serve"])?;
        until(Duration::from_secs(8), || {
            Ok(root.path().join("zor/control.sock").exists().then_some(()))
        })?;
        until(Duration::from_secs(8), || {
            let s = cli("zor", &["status"])?;
            Ok((s["snapshot"]["event_streams"] == count
                && s["snapshot"]["observations"]
                    .as_array()
                    .context("observations")?
                    .len()
                    == count)
                .then_some(()))
        })?;
        until(Duration::from_secs(8), || {
            for p in &proxies {
                if !p.snapshot()?.iter().any(|r| r["command"] == "capture") {
                    return Ok(None);
                }
            }
            Ok(Some(()))
        })?;
        std::thread::sleep(Duration::from_secs(1));
        let idle_start = epoch.elapsed().as_secs_f64();
        std::thread::sleep(Duration::from_millis(4200));
        let idle_end = epoch.elapsed().as_secs_f64();
        let burst_start = epoch.elapsed().as_secs_f64();
        for (upstream, pane) in &panes {
            completed(
                upstream,
                json!({"id":1,"command":"send-keys","pane":pane,"keys":"burst\\n"}),
            )?;
        }
        until(Duration::from_secs(8), || {
            for p in &proxies {
                if !p.snapshot()?.iter().any(|r| {
                    r["burst_done"] == true
                        && r["finished"].as_f64().is_some_and(|t| t >= burst_start)
                }) {
                    return Ok(None);
                }
            }
            Ok(Some(()))
        })?;
        let all_captured = epoch.elapsed().as_secs_f64();
        std::thread::sleep(Duration::from_secs(1));
        let burst_end = epoch.elapsed().as_secs_f64();
        let mut records = Vec::new();
        for (i, p) in proxies.iter().enumerate() {
            for mut r in p.snapshot()? {
                r["workspace"] = json!(i);
                records.push(r);
            }
        }
        let idle = crate::evidence::summarize_traffic(&records, idle_start, idle_end)?;
        let burst = crate::evidence::summarize_traffic(&records, burst_start, burst_end)?;
        ensure!(
            idle["captures"] == 0
                && burst["captures"].as_u64().context("captures")? >= count as u64,
            "capture activity"
        );
        Ok(
            json!({"panes":count,"idle":idle,"burst":burst,"all_burst_captured_ms":(all_captured-burst_start)*1000.,"boundaries":{"idle_start":idle_start,"idle_end":idle_end,"burst_start":burst_start,"burst_end":burst_end},"records":records}),
        )
    })();
    for p in &proxies {
        p.begin_shutdown();
    }
    let mut errors = Vec::new();
    for (name, owner) in owners.iter_mut().rev() {
        if *name == "zor" && owner.0.try_wait()?.is_none() {
            if let Err(e) = cli("zor", &["shutdown"]) {
                errors.push(format!("shutdown: {e:#}"));
            }
        } else if owner.0.try_wait()?.is_none()
            && let Err(e) = owner.0.terminate()
        {
            errors.push(e.to_string());
        }
        match process::wait(&mut owner.0, Duration::from_secs(10)) {
            Ok(s) => {
                if !s.success() {
                    errors.push(format!("{name}: {s}"));
                }
            }
            Err(e) => {
                errors.push(e.to_string());
                if let Err(e) = owner.0.kill() {
                    errors.push(e.to_string());
                }
                if let Err(e) = process::wait(&mut owner.0, Duration::from_secs(5)) {
                    errors.push(e.to_string());
                }
            }
        }
    }
    for p in &mut proxies {
        if let Err(e) = p.close() {
            errors.push(format!("proxy: {e:#}"));
        }
    }
    let workers = fs::read_dir(root.path().join("workers"))?
        .map(|p| -> Result<i32> { Ok(p?.file_name().to_str().context("pid filename")?.parse()?) })
        .collect::<Result<Vec<_>>>()?;
    if let Err(e) = until(Duration::from_secs(8), || {
        for pid in &workers {
            match nix::sys::signal::kill(nix::unistd::Pid::from_raw(*pid), None) {
                Err(nix::errno::Errno::ESRCH) => {}
                Ok(()) => return Ok(None),
                Err(e) => return Err(e.into()),
            }
        }
        Ok(Some(()))
    }) {
        errors.push(e.to_string());
    }
    ensure!(errors.is_empty(), "{}", errors.join("; "));
    let mut result = measured?;
    ensure!(workers.len() == count, "worker count");
    result["worker_count"] = json!(workers.len());
    result["worker_exit_confirmed"] = json!(true);
    let mut exits = serde_json::Map::new();
    for (name, owner) in &mut owners {
        exits.insert(
            (*name).into(),
            json!(owner.0.try_wait()?.context("owner running")?.code()),
        );
    }
    result["owners_exit"] = Value::Object(exits);
    crate::evidence::validate_traffic_case(&result)?;
    Ok(result)
}
pub fn run(args: Vec<String>) -> Result<()> {
    ensure!(
        args.len().is_multiple_of(2),
        "usage: capture-traffic --fux PATH --zor PATH --output NEW_JSON [--repetitions 1..3]"
    );
    let mut flags = BTreeMap::new();
    for p in args.chunks_exact(2) {
        ensure!(
            ["--fux", "--zor", "--output", "--repetitions"].contains(&p[0].as_str()),
            "unknown argument"
        );
        flags.insert(p[0].as_str(), p[1].as_str());
    }
    let repetitions = flags.get("--repetitions").unwrap_or(&"3").parse::<u64>()?;
    ensure!((1..=3).contains(&repetitions), "repetition bounds");
    let output = Path::new(flags.get("--output").context("missing output")?);
    ensure!(!output.exists(), "use new output file");
    let mut binaries = BTreeMap::new();
    let mut provenance =
        json!({"harness_kind":"rust-workspace-traffic","binaries":{},"sources":{}});
    for name in ["fux", "zor"] {
        let key = format!("--{name}");
        let p = Path::new(flags.get(key.as_str()).context("missing binary")?).canonicalize()?;
        provenance["binaries"][name] = json!({"path":p,"sha256":digest(&p)?});
        binaries.insert(name, p);
    }
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .context("root")?;
    for p in [
        "tools/xtask/src/traffic_capture.rs",
        "tools/xtask/src/traffic-worker.c",
        "tools/xtask/src/support/traffic_proxy.rs",
        "crates/zor/src/watch.rs",
        "crates/zor/src/observe.rs",
        "references/herdr/src/pane.rs",
    ] {
        provenance["sources"][p] = digest(&repository.join(p))?.into();
    }
    let temp = tempfile::Builder::new()
        .prefix("traffic-worker-rs-")
        .tempdir_in("/tmp")?;
    let source = temp.path().join("worker.c");
    fs::write(&source, include_bytes!("traffic-worker.c"))?;
    let worker = temp.path().join("claude");
    let mut c = Command::new("/usr/bin/clang");
    c.args(["-Wall", "-Wextra", "-Werror"])
        .arg(&source)
        .arg("-o")
        .arg(&worker);
    let compile = process::output(c, Duration::from_secs(30), 1024 * 1024)?;
    ensure!(
        compile.status.success(),
        "worker compile: {}",
        String::from_utf8_lossy(&compile.stderr)
    );
    let mut results = Vec::new();
    for repetition in 1..=repetitions {
        for count in [1, 4, 8] {
            let mut row = case(count, &binaries, &worker)?;
            row["repetition"] = json!(repetition);
            results.push(row);
            println!("{count} {repetition} passed");
            std::io::stdout().flush()?;
        }
    }
    let value = json!({"recorded_at":time::OffsetDateTime::now_utc().format(&time::format_description::well_known::Rfc3339)?,"provenance":provenance,"herdr_internal_reads":null,"herdr_measurement":"not exposed by equivalent workspace IPC; source uses in-process detection_text()","results":results});
    let mut bytes = serde_json::to_vec_pretty(&value)?;
    bytes.push(b'\n');
    fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(output)?
        .write_all(&bytes)?;
    Ok(())
}
