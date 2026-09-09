//! Paired owned-process idle/burst sampling using synthetic C workers.
use anyhow::{Context, Result, ensure};
use fux_xtask::support::{
    local::{Root, completed, until},
    process::{self, Guard, OwnedProcess},
    service,
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
fn number(v: &Value, key: &str) -> Result<u64> {
    v.get(key)
        .and_then(Value::as_u64)
        .with_context(|| format!("sample {key}"))
}
fn sample(owners: &mut [(&str, Guard)], sampler: &Path, epoch: Instant) -> Result<Value> {
    let mut rows = serde_json::Map::new();
    for (name, owner) in owners {
        ensure!(owner.0.try_wait()?.is_none(), "{name} exited");
        let mut c = Command::new(sampler);
        c.arg(owner.0.id().to_string());
        let r = process::output(c, Duration::from_secs(5), 1048576)?;
        ensure!(
            r.status.success(),
            "sample: {}",
            String::from_utf8_lossy(&r.stderr)
        );
        rows.insert((*name).into(), serde_json::from_slice(&r.stdout)?);
    }
    Ok(json!({"monotonic":epoch.elapsed().as_secs_f64(),"components":rows}))
}
fn interval(before: &Value, after: &Value) -> Result<Value> {
    let mut components = serde_json::Map::new();
    for (name, a) in before["components"].as_object().context("components")? {
        let b = &after["components"][name];
        ensure!(
            number(a, "start_abstime")? == number(b, "start_abstime")?
                && number(b, "start_abstime")? > 0,
            "PID reuse"
        );
        let ticks = i128::from(number(b, "user_ticks")?) + i128::from(number(b, "system_ticks")?)
            - i128::from(number(a, "user_ticks")?)
            - i128::from(number(a, "system_ticks")?);
        ensure!(ticks >= 0, "negative CPU delta");
        let cpu = ticks as f64 * number(b, "timebase_numer")? as f64
            / number(b, "timebase_denom")? as f64
            / 1e6;
        components.insert(
            name.clone(),
            json!({"cpu_ms":cpu,"rss_bytes":b["rss_bytes"],"footprint_bytes":b["footprint_bytes"]}),
        );
    }
    Ok(
        json!({"wall_ms":(after["monotonic"].as_f64().context("monotonic")? - before["monotonic"].as_f64().context("monotonic")?)*1000.,"components":components}),
    )
}
fn herdr(endpoint: &Path, method: &str, params: Value) -> Result<Value> {
    let v = service::rpc(
        endpoint,
        &json!({"id":"resources","method":method,"params":params}),
        Duration::from_secs(3),
    )?;
    ensure!(v.get("error").is_none(), "herdr: {v}");
    Ok(v.get("result").context("herdr result")?.clone())
}
fn case(
    backend: &str,
    count: usize,
    binaries: &BTreeMap<&str, PathBuf>,
    worker: &Path,
    sampler: &Path,
) -> Result<Value> {
    let mut root = Root::new(
        "resource-pair-rs-",
        &[worker.to_str().context("worker")?.into()],
    )?;
    fs::create_dir(root.path().join("workers"))?;
    for (k, p) in [("WORKER_DIR", "workers"), ("XDG_CACHE_HOME", "cache")] {
        root.env.insert(
            k.into(),
            root.path().join(p).to_str().context("root")?.into(),
        );
    }
    let log = tempfile::tempfile()?;
    let mut owners: Vec<(&str, Guard)> = Vec::new();
    let mut panes: Vec<(PathBuf, Value)> = Vec::new();
    let epoch = Instant::now();
    let cli = |name: &str, args: &[&str]| -> Result<Value> {
        let mut c = root.command(&binaries[name]);
        c.args(args);
        let r = process::output(c, Duration::from_secs(10), 1048576)?;
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
        if backend == "zor" {
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
                let endpoint = root.path().join("fux").join(format!("{name}.sock"));
                let listing = completed(&endpoint, json!({"id":1,"command":"list"}))?;
                for w in listing["workspaces"].as_array().context("workspaces")? {
                    for t in w["tabs"].as_array().context("tabs")? {
                        for p in t["panes"].as_array().context("panes")? {
                            panes.push((endpoint.clone(), p.get("id").context("pane ID")?.clone()));
                        }
                    }
                }
            }
            start("zor", &["serve"])?;
            until(Duration::from_secs(8), || {
                Ok(root.path().join("zor/control.sock").exists().then_some(()))
            })?;
            until(Duration::from_secs(8), || {
                Ok((cli("zor", &["status"])?["snapshot"]["observations"]
                    .as_array()
                    .context("observations")?
                    .len()
                    == count)
                    .then_some(()))
            })?;
        } else {
            for name in ["herdr", "herdr-dev"] {
                let path = root.path().join("config").join(name).join("config.toml");
                fs::create_dir_all(path.parent().context("config")?)?;
                fs::write(
                    &path,
                    format!(
                        "onboarding = false\n[terminal]\ndefault_shell = {}\nshell_mode = \"non_login\"\n[update]\nversion_check = false\nmanifest_check = false\n",
                        serde_json::to_string(worker)?
                    ),
                )?;
            }
            start("herdr", &["server"])?;
            let endpoint = until(Duration::from_secs(8), || {
                Ok(["herdr", "herdr-dev"]
                    .into_iter()
                    .map(|n| root.path().join("config").join(n).join("herdr.sock"))
                    .find(|p| p.exists()))
            })?;
            for _ in 0..count {
                herdr(
                    &endpoint,
                    "workspace.create",
                    json!({"cwd":root.path(),"focus":true}),
                )?;
            }
            for p in herdr(&endpoint, "pane.list", json!({}))?["panes"]
                .as_array()
                .context("panes")?
            {
                panes.push((
                    endpoint.clone(),
                    p.get("pane_id").context("pane ID")?.clone(),
                ));
            }
            until(Duration::from_secs(8), || {
                Ok((herdr(&endpoint, "agent.list", json!({}))?["agents"]
                    .as_array()
                    .context("agents")?
                    .len()
                    == count)
                    .then_some(()))
            })?;
        }
        ensure!(panes.len() == count, "pane count");
        until(Duration::from_secs(8), || {
            Ok((fs::read_dir(root.path().join("workers"))?.count() == count).then_some(()))
        })?;
        let capture = |(endpoint, pane): &(PathBuf, Value)| -> Result<Value> {
            if backend == "zor" {
                completed(
                    endpoint,
                    json!({"id":1,"command":"capture","pane":pane,"max_bytes":131072}),
                )
            } else {
                herdr(
                    endpoint,
                    "pane.read",
                    json!({"pane_id":pane,"source":"visible","strip_ansi":true}),
                )
            }
        };
        for pane in &panes {
            until(Duration::from_secs(8), || {
                Ok(capture(pane)?.to_string().contains("READY").then_some(()))
            })?;
        }
        let initial = panes.iter().map(&capture).collect::<Result<Vec<_>>>()?;
        std::thread::sleep(Duration::from_secs(1));
        let idle_before = sample(&mut owners, sampler, epoch)?;
        std::thread::sleep(Duration::from_secs(3));
        let idle_after = sample(&mut owners, sampler, epoch)?;
        let burst_before = sample(&mut owners, sampler, epoch)?;
        let started = Instant::now();
        for (endpoint, pane) in &panes {
            if backend == "zor" {
                completed(
                    endpoint,
                    json!({"id":1,"command":"send-keys","pane":pane,"keys":"burst\\n"}),
                )?;
            } else {
                herdr(
                    endpoint,
                    "pane.send_input",
                    json!({"pane_id":pane,"text":"burst\n"}),
                )?;
            }
        }
        let mut reads = 0;
        for pane in &panes {
            until(Duration::from_secs(8), || {
                reads += 1;
                Ok(capture(pane)?
                    .to_string()
                    .contains("BURST_DONE")
                    .then_some(()))
            })?;
        }
        let visible_ms = started.elapsed().as_secs_f64() * 1000.;
        std::thread::sleep(Duration::from_secs(1));
        let burst_after = sample(&mut owners, sampler, epoch)?;
        let mut paths = fs::read_dir(root.path().join("workers"))?
            .map(|e| e.map(|e| e.path()))
            .collect::<std::io::Result<Vec<_>>>()?;
        paths.sort();
        let viewports = paths
            .iter()
            .map(|p| -> Result<Vec<u64>> {
                fs::read_to_string(p)?
                    .split_whitespace()
                    .map(|s| s.parse().map_err(Into::into))
                    .collect()
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(
            json!({"backend":backend,"panes":count,"idle":interval(&idle_before,&idle_after)?,"burst":interval(&burst_before,&burst_after)?,"burst_all_visible_ms":visible_ms,"harness_capture_reads":reads,"initial_captures":initial,"samples":[idle_before,idle_after,burst_before,burst_after],"worker_viewports":viewports}),
        )
    })();
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
    result["worker_exit_confirmed"] = json!(true);
    let mut exits = serde_json::Map::new();
    for (name, owner) in &mut owners {
        exits.insert(
            (*name).into(),
            json!(owner.0.try_wait()?.context("owner running")?.code()),
        );
    }
    result["owners_exit"] = Value::Object(exits);
    crate::evidence::validate_resource_case(&result)?;
    Ok(result)
}
pub fn run(args: Vec<String>) -> Result<()> {
    ensure!(
        args.len().is_multiple_of(2),
        "usage: capture-resources --fux PATH --zor PATH --herdr PATH --herdr-provenance JSON --output NEW_JSON [--repetitions 1..3]"
    );
    let mut flags = BTreeMap::new();
    for p in args.chunks_exact(2) {
        ensure!(
            [
                "--fux",
                "--zor",
                "--herdr",
                "--herdr-provenance",
                "--output",
                "--repetitions"
            ]
            .contains(&p[0].as_str()),
            "unknown argument"
        );
        flags.insert(p[0].as_str(), p[1].as_str());
    }
    let repetitions = flags.get("--repetitions").unwrap_or(&"3").parse::<u64>()?;
    ensure!((1..=3).contains(&repetitions), "repetition bounds");
    let output = Path::new(flags.get("--output").context("missing output")?);
    ensure!(!output.exists(), "use new output file");
    let mut binaries = BTreeMap::new();
    let mut provenance = json!({"harness_kind":"rust-owned-resources","binaries":{},"sources":{}});
    for name in ["fux", "zor", "herdr"] {
        let key = format!("--{name}");
        let p = Path::new(flags.get(key.as_str()).context("missing binary")?).canonicalize()?;
        provenance["binaries"][name] = json!({"path":p,"sha256":digest(&p)?});
        binaries.insert(name, p);
    }
    provenance["herdr_build"] = serde_json::from_slice(&fs::read(
        flags
            .get("--herdr-provenance")
            .context("missing herdr provenance")?,
    )?)?;
    ensure!(
        provenance["herdr_build"]
            .get("binary_sha256")
            .context("herdr build hash")?
            == &provenance["binaries"]["herdr"]["sha256"],
        "herdr binary provenance"
    );
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .context("repository")?;
    for path in [
        "tools/xtask/src/resource_capture.rs",
        "tools/xtask/src/traffic-worker.c",
        "tools/xtask/src/support/local.rs",
        "tools/xtask/src/support/service.rs",
        "tools/xtask/src/support/process.rs",
        "tools/comparisons/resource_sampler.c",
        "zor/src/watch.rs",
        "zor/src/service.rs",
        "src/ecs/systems/output.rs",
    ] {
        provenance["sources"][path] = digest(&repository.join(path))?.into();
    }
    let root = tempfile::Builder::new()
        .prefix("resource-worker-rs-")
        .tempdir_in("/tmp")?;
    let source = root.path().join("worker.c");
    fs::write(&source, include_bytes!("traffic-worker.c"))?;
    let worker = root.path().join("claude");
    let sampler = root.path().join("sample");
    for (src, bin) in [
        (source, worker.clone()),
        (
            repository.join("tools/comparisons/resource_sampler.c"),
            sampler.clone(),
        ),
    ] {
        let mut c = Command::new("/usr/bin/clang");
        c.args(["-Wall", "-Wextra", "-Werror"])
            .arg(src)
            .arg("-o")
            .arg(bin);
        let r = process::output(c, Duration::from_secs(30), 1048576)?;
        ensure!(
            r.status.success(),
            "compile: {}",
            String::from_utf8_lossy(&r.stderr)
        );
    }
    let mut c = Command::new(&sampler);
    c.arg("--calibrate");
    let r = process::output(c, Duration::from_secs(5), 1048576)?;
    ensure!(r.status.success(), "calibration failed");
    let calibration: Value = serde_json::from_slice(&r.stdout)?;
    let mut results = Vec::new();
    for repetition in 1..=repetitions {
        for count in [1, 4, 8] {
            for backend in ["zor", "herdr"] {
                let mut row = case(backend, count, &binaries, &worker, &sampler)?;
                row["repetition"] = json!(repetition);
                results.push(row);
                println!("{backend} {count} {repetition} passed");
                std::io::stdout().flush()?;
            }
        }
    }
    let mut c = Command::new("/usr/bin/uname");
    c.arg("-a");
    let r = process::output(c, Duration::from_secs(5), 65536)?;
    ensure!(r.status.success(), "platform identification");
    let value = json!({"schema":1,"recorded_at":time::OffsetDateTime::now_utc().format(&time::format_description::well_known::Rfc3339)?,"platform":String::from_utf8(r.stdout)?.trim(),"provenance":provenance,"calibration":calibration,"results":results});
    let mut bytes = serde_json::to_vec_pretty(&value)?;
    bytes.push(b'\n');
    fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(output)?
        .write_all(&bytes)?;
    Ok(())
}
