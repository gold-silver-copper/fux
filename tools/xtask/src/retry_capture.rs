//! Retry of lost request/reply: correlation IDs and durable operation IDs differ.
use crate::prompt_capture::{Harness, digest};
use anyhow::{Context, Result, ensure};
use fux_xtask::support::{
    local::{Root, completed, until},
    process,
    retry_proxy::{Proxy, raw_prompt},
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};
fn worker_ready(h: &Harness) -> Result<()> {
    until(Duration::from_secs(8), || {
        let p = h.root.path().join("worker.pid");
        if !p.exists() {
            return Ok(None);
        }
        let s = fs::read_to_string(p)?;
        Ok((!s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())).then_some(()))
    })
}
fn herdr_retry(h: &Harness, p: &Proxy) -> Result<Value> {
    ensure!(
        h.call(
            "workspace.create",
            json!({"cwd":h.root.path(),"focus":true})
        )?
        .get("error")
        .is_none(),
        "workspace"
    );
    let v = h.call("pane.list", json!({}))?;
    let panes = v
        .pointer("/result/panes")
        .and_then(Value::as_array)
        .context("panes")?;
    ensure!(panes.len() == 1, "one pane");
    let pane = panes[0].get("pane_id").context("pane ID")?;
    worker_ready(h)?;
    ensure!(h.call("pane.report_agent",json!({"pane_id":pane,"agent":"claude","source":"comparison:fixture","state":"idle","seq":1}))?.get("error").is_none(),"idle report");
    until(Duration::from_secs(8), || {
        Ok((h
            .call("agent.get", json!({"target":pane}))?
            .pointer("/result/agent/agent_status")
            == Some(&json!("idle")))
        .then_some(()))
    })?;
    let request = json!({"id":"stable-request-id","method":"agent.prompt","params":{"target":pane,"text":"retry-probe"}});
    let first = raw_prompt(&p.path, &request)?;
    ensure!(
        first == json!({"transport":"eof"}),
        "first response: {first}"
    );
    let before = h.received()?;
    let retry = raw_prompt(&p.path, &request)?;
    ensure!(
        retry.pointer("/result/type") == Some(&json!("agent_prompted")),
        "retry: {retry}"
    );
    Ok(json!({"first":first,"retry":retry,"input_before_retry":before}))
}
fn zor_retry(h: &Harness, p: &Proxy, zor: &Path) -> Result<Value> {
    macro_rules! t{($($a:expr),*$(,)?)=>{h.task(zor,&[$($a),*],true)?};}
    let listing = completed(&h.control, json!({"id":1,"command":"list"}))?;
    let pane = listing
        .pointer("/workspaces/0/tabs/0/panes/0/id")
        .context("pane")?;
    worker_ready(h)?;
    t!(
        "adopt",
        "worker",
        "--title",
        "retry comparison",
        "--instance",
        listing["instance"].as_str().context("instance")?,
        "--workspace",
        "default",
        "--pane",
        &pane.to_string(),
        "--runtime",
        p.path
            .parent()
            .context("proxy parent")?
            .to_str()
            .context("runtime")?
    );
    t!(
        "prepare",
        "worker",
        "--operation",
        "stable-operation-id",
        "--text",
        "retry-probe",
        "--timeout-ms",
        "30000"
    );
    let reserved = t!("reserve", "stable-operation-id");
    let operation = reserved
        .pointer("/receipt/operation")
        .context("operation")?;
    let first = h.task(zor, &["submit", "stable-operation-id"], false)?;
    let before = h.received()?;
    let failed = t!("inspect", "worker")["prompts"][0]["delivery"].clone();
    t!("submit", "stable-operation-id");
    let retried = until(Duration::from_secs(8), || {
        let v = t!("reconcile", "stable-operation-id");
        Ok((v["delivery"] == "delivered").then_some(v))
    })?;
    ensure!(
        retried.pointer("/receipt/operation") == Some(operation) && retried["wait"] == "pending",
        "retry changed operation or inferred response"
    );
    ensure!(
        t!("inspect", "worker")["task"]["outcome"] == "open",
        "inferred success"
    );
    Ok(
        json!({"first":first,"first_delivery":failed,"retry":{"delivery":retried["delivery"],"wait":retried["wait"],"operation":operation},"input_before_retry":before}),
    )
}
fn case(
    backend: &str,
    fault: &str,
    binaries: &BTreeMap<&str, PathBuf>,
    worker: &Path,
) -> Result<Value> {
    let mut h = Harness::start(
        backend,
        &binaries[if backend == "herdr" { "herdr" } else { "fux" }],
        worker,
    )?;
    let mut proxy = None;
    let measured = (|| -> Result<Value> {
        fs::create_dir(h.root.path().join("proxy"))?;
        proxy = Some(Proxy::start(
            &h.root.path().join("proxy/default.sock"),
            &h.control,
            backend,
            fault,
            &h.root.path().join("received"),
        )?);
        let p = proxy.as_ref().unwrap();
        let mut value = if backend == "herdr" {
            herdr_retry(&h, p)?
        } else {
            zor_retry(&h, p, &binaries["zor"])?
        };
        ensure!(p.dropped(), "fault not injected");
        ensure!(
            value["input_before_retry"]
                == if fault == "before-forward" {
                    ""
                } else {
                    "retry-probe\n"
                },
            "input before retry: {value}"
        );
        let count = if backend == "herdr" && fault == "after-consumption" {
            2
        } else {
            1
        };
        until(Duration::from_secs(8), || {
            Ok((h.received()? == "retry-probe\n".repeat(count)).then_some(()))
        })?;
        let events = p.events();
        let submissions = events
            .iter()
            .filter(|e| {
                e["method"]
                    == if backend == "herdr" {
                        "agent.prompt"
                    } else {
                        "input-submit"
                    }
            })
            .collect::<Vec<_>>();
        if backend == "herdr" {
            ensure!(
                submissions.len() == 2
                    && submissions
                        .iter()
                        .map(|e| e["request_sha256"].as_str().context("request hash"))
                        .collect::<Result<BTreeSet<_>>>()?
                        .len()
                        == 1,
                "request retry differs"
            );
        } else {
            ensure!(
                submissions.len() == if fault == "before-forward" { 2 } else { 1 },
                "submit count"
            );
            ensure!(
                submissions
                    .iter()
                    .all(|e| e.get("operation") == value.pointer("/retry/operation")),
                "operation retry differs"
            );
        }
        value["received"] = json!(h.received()?);
        value["input_count"] = json!(count);
        value["proxy_events"] = json!(events);
        Ok(value)
    })();
    let proxy_result = proxy.as_mut().map_or(Ok(()), Proxy::close);
    if measured.is_err() || proxy_result.is_err() {
        h.diagnostic()?;
    }
    let cleanup = h.finish();
    if let Err(e) = &proxy_result {
        eprintln!("proxy: {e:#}");
    }
    if let Err(e) = &cleanup {
        eprintln!("cleanup: {e:#}");
    }
    let mut value = measured?;
    proxy_result?;
    let exit = cleanup?;
    let count = usize::try_from(value["input_count"].as_u64().context("input count")?)?;
    ensure!(
        json!(h.received()?) == value["received"] && h.received()? == "retry-probe\n".repeat(count),
        "final input changed"
    );
    for (key, v) in [
        ("final_input_verified", json!(true)),
        ("backend", json!(backend)),
        ("fault", json!(fault)),
        ("server_exit", json!(exit)),
        ("worker_exit_confirmed", json!(true)),
    ] {
        value[key] = v;
    }
    Ok(value)
}
pub fn run(args: Vec<String>) -> Result<()> {
    ensure!(
        args.len().is_multiple_of(2),
        "usage: capture-input-retry --herdr PATH --fux PATH --zor PATH --output NEW_JSON [--repetitions 1..20] [--herdr-reference DIR]"
    );
    let mut flags = BTreeMap::new();
    for p in args.chunks_exact(2) {
        ensure!(
            [
                "--herdr",
                "--fux",
                "--zor",
                "--output",
                "--repetitions",
                "--herdr-reference"
            ]
            .contains(&p[0].as_str()),
            "unknown argument"
        );
        flags.insert(p[0].as_str(), p[1].as_str());
    }
    let repetitions = flags.get("--repetitions").unwrap_or(&"3").parse::<u64>()?;
    ensure!((1..=20).contains(&repetitions), "repetition bounds");
    let output = Path::new(flags.get("--output").context("output")?);
    ensure!(!output.exists(), "use new output file");
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .context("repository")?;
    let root = Root::new("retry-worker-rs-", &["/bin/cat".into()])?;
    let mut binaries = BTreeMap::new();
    let mut provenance = json!({"harness_kind":"rust-input-retry","sources":{}});
    for name in ["herdr", "fux", "zor"] {
        let key = format!("--{name}");
        let p = Path::new(flags.get(key.as_str()).context("binary")?).canonicalize()?;
        let mut c = root.command(&p);
        c.arg("--version");
        let r = process::output(c, Duration::from_secs(5), 65536)?;
        ensure!(r.status.success(), "version");
        provenance[name] =
            json!({"sha256":digest(&p)?,"version":String::from_utf8(r.stdout)?.trim()});
        binaries.insert(name, p);
    }
    ensure!(
        provenance["herdr"]["version"]
            .as_str()
            .context("version")?
            .starts_with("herdr 0.8.2"),
        "use 0.8.2 reference"
    );
    let reference = flags
        .get("--herdr-reference")
        .map(PathBuf::from)
        .unwrap_or_else(|| repository.join("references/herdr"));
    let mut c = Command::new("/usr/bin/git");
    c.arg("-C").arg(reference).args(["rev-parse", "HEAD"]);
    let r = process::output(c, Duration::from_secs(5), 65536)?;
    ensure!(r.status.success(), "reference commit");
    provenance["reference_commit"] = json!(String::from_utf8(r.stdout)?.trim());
    let mut c = Command::new("/usr/bin/uname");
    c.arg("-a");
    let r = process::output(c, Duration::from_secs(5), 65536)?;
    ensure!(r.status.success(), "platform");
    provenance["platform"] = json!(String::from_utf8(r.stdout)?.trim());
    provenance["harness_sha256"] = json!(digest(
        &repository.join("tools/xtask/src/retry_capture.rs")
    )?);
    provenance["helper_sha256"] = json!(digest(
        &repository.join("tools/xtask/src/prompt_capture.rs")
    )?);
    provenance["worker_source_sha256"] = json!(format!(
        "{:x}",
        Sha256::digest(include_bytes!("prompt-worker.c"))
    ));
    for p in [
        "tools/xtask/src/retry_capture.rs",
        "tools/xtask/src/prompt_capture.rs",
        "tools/xtask/src/prompt-worker.c",
        "tools/xtask/src/support/retry_proxy.rs",
        "tools/xtask/src/support/local.rs",
        "tools/xtask/src/support/service.rs",
        "tools/xtask/src/support/process.rs",
    ] {
        provenance["sources"][p] = json!(digest(&repository.join(p))?);
    }
    let source = root.path().join("worker.c");
    fs::write(&source, include_bytes!("prompt-worker.c"))?;
    let worker = root.path().join("claude");
    let mut c = root.command(Path::new("/usr/bin/clang"));
    c.arg(&source).arg("-o").arg(&worker);
    let r = process::output(c, Duration::from_secs(30), 1048576)?;
    ensure!(
        r.status.success(),
        "worker compile: {}",
        String::from_utf8_lossy(&r.stderr)
    );
    let mut results = Vec::new();
    for repetition in 1..=repetitions {
        for fault in ["before-forward", "after-consumption"] {
            for backend in ["herdr", "zor"] {
                let mut row = case(backend, fault, &binaries, &worker)?;
                row["repetition"] = json!(repetition);
                results.push(row);
                println!("{backend} {fault} {repetition} passed");
                std::io::stdout().flush()?;
            }
        }
    }
    let mut bytes = serde_json::to_vec_pretty(
        &json!({"provenance":provenance,"synthetic":true,"results":results}),
    )?;
    bytes.push(b'\n');
    fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(output)?
        .write_all(&bytes)?;
    Ok(())
}
