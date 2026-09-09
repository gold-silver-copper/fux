//! Synthetic worker continuity across controller loss and PTY-owner loss.
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
    io::{Read, Seek, SeekFrom, Write},
    os::unix::process::ExitStatusExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant},
};
fn digest(p: &Path) -> Result<String> {
    Ok(format!("{:x}", Sha256::digest(fs::read(p)?)))
}
fn alive(pid: i32) -> Result<bool> {
    ensure!(pid > 1, "invalid worker PID");
    match nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None) {
        Ok(()) => Ok(true),
        Err(nix::errno::Errno::ESRCH) => Ok(false),
        Err(e) => Err(e.into()),
    }
}
fn stop(child: &mut Option<Guard>) -> Result<()> {
    if let Some(c) = child
        && c.0.try_wait()?.is_none()
    {
        c.0.terminate()?;
        match process::wait(&mut c.0, Duration::from_secs(10)) {
            Ok(s) => ensure!(s.success(), "cleanup exit: {s}"),
            Err(e) => {
                c.0.kill()?;
                process::wait(&mut c.0, Duration::from_secs(3))?;
                return Err(e.context("cleanup required forced termination"));
            }
        }
    }
    Ok(())
}
fn transient(e: &anyhow::Error) -> bool {
    e.chain().any(|cause| {
        cause.downcast_ref::<std::io::Error>().is_some_and(|e| {
            matches!(
                e.kind(),
                std::io::ErrorKind::NotFound
                    | std::io::ErrorKind::ConnectionRefused
                    | std::io::ErrorKind::ConnectionReset
            )
        }) || cause.downcast_ref::<nix::errno::Errno>().is_some_and(|e| {
            matches!(
                e,
                nix::errno::Errno::ENOENT
                    | nix::errno::Errno::ECONNREFUSED
                    | nix::errno::Errno::ECONNRESET
            )
        })
    })
}
fn start_service(
    root: &Root,
    zor: &Path,
    log: &fs::File,
    owner: &mut Option<Guard>,
    previous: Option<&Value>,
) -> Result<Value> {
    let mut c = root.command(zor);
    c.arg("serve")
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log.try_clone()?);
    *owner = Some(Guard(c.spawn()?));
    until(Duration::from_secs(8), || {
        ensure!(
            owner.as_mut().unwrap().0.try_wait()?.is_none(),
            "service exited before readiness"
        );
        match service::rpc(
            &root.path().join("zor/control.sock"),
            &json!({"v":1,"id":1,"op":"ping"}),
            Duration::from_secs(10),
        ) {
            Ok(v) => {
                let instance = v.get("service_instance").context("service instance")?;
                Ok((Some(instance) != previous).then_some(instance.clone()))
            }
            Err(e) if transient(&e) => Ok(None),
            Err(e) => Err(e),
        }
    })
}
fn task(endpoint: &Path, instance: &Value, payload: Value) -> Result<Value> {
    let v = service::rpc(
        endpoint,
        &json!({"v":1,"id":1,"op":"task","service_instance":instance,"task":payload}),
        Duration::from_secs(10),
    )?;
    ensure!(v["status"] == "completed", "task reply: {v}");
    Ok(v.get("value").context("task value")?.clone())
}
fn direct(endpoint: &Path, method: &str, params: Value) -> Result<Value> {
    let v = service::rpc(
        endpoint,
        &json!({"id":"fixture","method":method,"params":params}),
        Duration::from_secs(10),
    )?;
    ensure!(v.get("error").is_none(), "herdr reply: {v}");
    Ok(v.get("result").context("herdr result")?.clone())
}
fn case(scope: &str, binaries: &BTreeMap<&str, PathBuf>, worker: &Path) -> Result<Value> {
    let mut root = Root::new(
        "svc-fault-rs-",
        &[worker.to_str().context("worker")?.into()],
    )?;
    for (key, name) in [
        ("WORKER_LOG", "received"),
        ("WORKER_PID", "worker.pid"),
        ("WORKER_RESPONSE", "responded"),
    ] {
        root.env.insert(
            key.into(),
            root.path().join(name).to_str().context("root")?.into(),
        );
    }
    let herdr = scope == "herdr-server";
    if herdr {
        for name in ["herdr", "herdr-dev"] {
            let p = root.path().join("config").join(name).join("config.toml");
            fs::create_dir_all(p.parent().context("config")?)?;
            fs::write(
                p,
                format!(
                    "onboarding = false\n[terminal]\ndefault_shell = {}\nshell_mode = \"non_login\"\n[update]\nversion_check = false\nmanifest_check = false\n",
                    serde_json::to_string(worker)?
                ),
            )?;
        }
    }
    let mut server = None;
    let mut service_owner = None;
    let mut pid = None;
    let mut log = tempfile::tempfile()?;
    let mut result = json!({"fault":scope,"signal":"SIGKILL","session_resume_tested":false});
    let measured = (|| -> Result<()> {
        let (tool, arg) = if herdr {
            ("herdr", "server")
        } else {
            ("fux", "serve")
        };
        let mut c = root.command(&binaries[tool]);
        c.arg(arg)
            .stdin(Stdio::null())
            .stdout(log.try_clone()?)
            .stderr(log.try_clone()?);
        let worker_earliest = Instant::now();
        server = Some(Guard(c.spawn()?));
        let control = until(Duration::from_secs(8), || {
            ensure!(
                server.as_mut().unwrap().0.try_wait()?.is_none(),
                "server exited before readiness"
            );
            let paths = if herdr {
                ["herdr", "herdr-dev"]
                    .map(|n| root.path().join("config").join(n).join("herdr.sock"))
                    .to_vec()
            } else {
                vec![root.control()]
            };
            Ok(paths.into_iter().find(|p| p.exists()))
        })?;
        let mut instance = Value::Null;
        let pane = if herdr {
            direct(
                &control,
                "workspace.create",
                json!({"cwd":root.path(),"focus":true}),
            )?;
            let v = direct(&control, "pane.list", json!({}))?;
            let panes = v["panes"].as_array().context("panes")?;
            ensure!(panes.len() == 1, "one worker pane");
            panes[0].get("pane_id").context("pane ID")?.clone()
        } else {
            let listing = completed(&control, json!({"id":1,"command":"list"}))?;
            instance = listing.get("instance").context("fux instance")?.clone();
            listing
                .pointer("/workspaces/0/tabs/0/panes/0/id")
                .context("pane")?
                .clone()
        };
        let worker_pid = until(Duration::from_secs(8), || {
            let path = root.path().join("worker.pid");
            if !path.exists() {
                return Ok(None);
            }
            let text = fs::read_to_string(path)?;
            Ok(
                (!text.is_empty() && text.bytes().all(|b| b.is_ascii_digit()))
                    .then(|| text.parse::<i32>())
                    .transpose()?,
            )
        })?;
        pid = Some(worker_pid);
        ensure!(alive(worker_pid)?, "worker not alive");
        result["worker_pid"] = json!(worker_pid);
        let endpoint = root.path().join("zor/control.sock");
        let mut service_instance = Value::Null;
        let mut adopted = Value::Null;
        let mut prepared = Value::Null;
        let mut delivered = Value::Null;
        if herdr {
            direct(
                &control,
                "pane.report_agent",
                json!({"pane_id":pane,"agent":"claude","source":"comparison:fixture","state":"idle","seq":1}),
            )?;
            until(Duration::from_secs(8), || {
                Ok((direct(&control,"agent.get",json!({"target":pane}))?["agent"]["agent_status"] == "idle").then_some(()))
            })?;
            direct(
                &control,
                "agent.prompt",
                json!({"target":pane,"text":"before-crash"}),
            )?;
        } else {
            service_instance =
                start_service(&root, &binaries["zor"], &log, &mut service_owner, None)?;
            adopted = task(
                &endpoint,
                &service_instance,
                json!({"action":"adopt","id":"worker","title":"failure comparison","instance":instance,"workspace":"default","pane":pane}),
            )?;
            prepared = task(
                &endpoint,
                &service_instance,
                json!({"action":"prepare","id":"worker","operation":"before","text":"before-crash","timeout_ms":60000}),
            )?;
            task(
                &endpoint,
                &service_instance,
                json!({"action":"submit","operation":"before"}),
            )?;
            delivered = until(Duration::from_secs(8), || {
                let v = task(
                    &endpoint,
                    &service_instance,
                    json!({"action":"reconcile","operation":"before"}),
                )?;
                Ok((v["delivery"] == "delivered").then_some(v))
            })?;
            ensure!(delivered["wait"] == "pending", "delivery is not completion");
        }
        for (file, expected) in [("received", "before-crash\n"), ("responded", "response\n")] {
            until(Duration::from_secs(8), || {
                let p = root.path().join(file);
                Ok((p.exists() && fs::read_to_string(p)? == expected).then_some(()))
            })?;
        }
        result["input_before_crash"] = json!(fs::read_to_string(root.path().join("received"))?);
        result["setup_seconds"] = json!(worker_earliest.elapsed().as_secs_f64());
        ensure!(
            worker_earliest.elapsed() < Duration::from_secs(30),
            "alarm could account for crash exit"
        );
        let victim = if scope == "zor-service" {
            service_owner.as_mut().unwrap()
        } else {
            server.as_mut().unwrap()
        };
        victim.0.kill()?;
        let exit = process::wait(&mut victim.0, Duration::from_secs(5))?;
        ensure!(
            exit.signal() == Some(libc::SIGKILL),
            "fault did not kill owned victim: {exit}"
        );
        result["fault_exit"] = json!(-libc::SIGKILL);
        if scope == "zor-service" {
            ensure!(
                alive(worker_pid)? && server.as_mut().unwrap().0.try_wait()?.is_none(),
                "controller loss killed worker/server"
            );
            service_instance = start_service(
                &root,
                &binaries["zor"],
                &log,
                &mut service_owner,
                Some(&service_instance),
            )?;
            let retained = task(
                &endpoint,
                &service_instance,
                json!({"action":"inspect","id":"worker"}),
            )?;
            ensure!(
                retained.get("session").context("retained session")?
                    == adopted.get("session").context("adopted session")?,
                "session changed"
            );
            let reconciled = task(
                &endpoint,
                &service_instance,
                json!({"action":"reconcile","operation":"before"}),
            )?;
            ensure!(
                reconciled.get("receipt").context("retained receipt")?
                    == delivered.get("receipt").context("delivered receipt")?
                    && reconciled["wait"] == "pending",
                "receipt changed"
            );
            task(
                &endpoint,
                &service_instance,
                json!({"action":"submit","operation":"before"}),
            )?;
            task(
                &endpoint,
                &service_instance,
                json!({"action":"report","operation":"before","token":prepared["report_token"],"producer":"comparison-fixture","sequence":1,"input_operation":delivered["receipt"]["operation"],"kind":"response-observed"}),
            )?;
            ensure!(
                task(
                    &endpoint,
                    &service_instance,
                    json!({"action":"wait","operation":"before"})
                )?["wait"]
                    == "response-observed",
                "fresh report wait"
            );
            task(
                &endpoint,
                &service_instance,
                json!({"action":"prepare","id":"worker","operation":"after","text":"after-crash","timeout_ms":60000}),
            )?;
            task(
                &endpoint,
                &service_instance,
                json!({"action":"submit","operation":"after"}),
            )?;
            until(Duration::from_secs(8), || {
                Ok((fs::read_to_string(root.path().join("received"))?
                    == "before-crash\nafter-crash\n")
                    .then_some(()))
            })?;
            ensure!(
                fs::read_to_string(root.path().join("worker.pid"))? == worker_pid.to_string()
                    && alive(worker_pid)?,
                "worker identity changed"
            );
            ensure!(
                task(
                    &endpoint,
                    &service_instance,
                    json!({"action":"inspect","id":"worker"})
                )?["task"]["outcome"]
                    == "open",
                "inferred task success"
            );
            for (key, value) in [
                ("worker_survived", json!(true)),
                ("service_instance_changed", json!(true)),
                ("session_retained", json!(true)),
                ("receipt_retained", json!(true)),
                ("retry_duplicated_input", json!(false)),
                ("fresh_report_wait", json!("response-observed")),
                ("task_outcome", json!("open")),
            ] {
                result[key] = value;
            }
        } else {
            until(Duration::from_secs(8), || {
                Ok((!alive(worker_pid)?).then_some(()))
            })?;
            result["worker_survived"] = json!(false);
        }
        result["input_after_fault"] = json!(fs::read_to_string(root.path().join("received"))?);
        Ok(())
    })();
    let mut errors = Vec::new();
    for owner in [&mut service_owner, &mut server] {
        if let Err(e) = stop(owner) {
            errors.push(format!("{e:#}"));
        }
    }
    if let Some(pid) = pid
        && let Err(e) = until(Duration::from_secs(47), || Ok((!alive(pid)?).then_some(())))
    {
        errors.push(format!("{e:#}"));
    }
    if measured.is_err() || !errors.is_empty() {
        log.seek(SeekFrom::Start(0))?;
        let mut bytes = Vec::new();
        (&mut log).take(16384).read_to_end(&mut bytes)?;
        eprintln!("{}", String::from_utf8_lossy(&bytes));
        if !errors.is_empty() {
            eprintln!("cleanup: {}", errors.join("; "));
        }
    }
    measured?;
    ensure!(errors.is_empty(), "cleanup: {}", errors.join("; "));
    ensure!(
        json!(fs::read_to_string(root.path().join("received"))?) == result["input_after_fault"],
        "cleanup caused input"
    );
    result["cleanup_worker_exit_confirmed"] = json!(true);
    Ok(result)
}
pub fn run(args: Vec<String>) -> Result<()> {
    ensure!(
        args.len().is_multiple_of(2),
        "usage: capture-service-failure --herdr PATH --fux PATH --zor PATH --output NEW_JSON [--repetitions 1..20] [--herdr-provenance BUILD_JSON]"
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
                "--herdr-provenance"
            ]
            .contains(&p[0].as_str()),
            "unknown argument"
        );
        flags.insert(p[0].as_str(), p[1].as_str());
    }
    let repetitions = flags.get("--repetitions").unwrap_or(&"3").parse::<u64>()?;
    ensure!((1..=20).contains(&repetitions), "repetition bounds");
    let output = Path::new(flags.get("--output").context("missing output")?);
    ensure!(!output.exists(), "use a new output file");
    let mut binaries = BTreeMap::new();
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .context("repository")?;
    let mut provenance = json!({"harness_kind":"rust-service-failure","harness_sha256":digest(&repository.join("tools/xtask/src/service_failure_capture.rs"))?,"sources":{},"binaries":{}});
    for name in ["herdr", "fux", "zor"] {
        let key = format!("--{name}");
        let p = Path::new(flags.get(key.as_str()).context("missing binary")?).canonicalize()?;
        provenance["binaries"][name] = json!(digest(&p)?);
        binaries.insert(name, p);
    }
    let reference_path = flags
        .get("--herdr-provenance")
        .map(PathBuf::from)
        .unwrap_or_else(|| repository.join("docs/comparisons/prompt-boundary-build.json"));
    let reference: Value = serde_json::from_slice(&fs::read(&reference_path)?)?;
    ensure!(
        reference
            .get("binary_sha256")
            .context("reference binary hash")?
            == &provenance["binaries"]["herdr"],
        "use the verified reference build"
    );
    provenance["herdr_reference_commit"] = reference
        .get("reference_commit")
        .context("reference commit")?
        .clone();
    provenance["herdr_build"] = reference;
    let mut c = Command::new("/usr/bin/uname");
    c.arg("-a");
    let r = process::output(c, Duration::from_secs(5), 65536)?;
    ensure!(r.status.success(), "platform identification");
    provenance["platform"] = json!(String::from_utf8(r.stdout)?.trim());
    for p in [
        "tools/xtask/src/service_failure_capture.rs",
        "tools/xtask/src/prompt-worker.c",
        "tools/xtask/src/support/local.rs",
        "tools/xtask/src/support/service.rs",
        "tools/xtask/src/support/process.rs",
    ] {
        provenance["sources"][p] = json!(digest(&repository.join(p))?);
    }
    let worker_source = include_str!("prompt-worker.c").replace(
        "    char line[4096];",
        "    alarm(45);\n    char line[4096];",
    );
    provenance["worker_source_sha256"] =
        json!(format!("{:x}", Sha256::digest(worker_source.as_bytes())));
    let root = tempfile::Builder::new()
        .prefix("svc-worker-rs-")
        .tempdir_in("/tmp")?;
    let source = root.path().join("worker.c");
    fs::write(&source, worker_source)?;
    let worker = root.path().join("claude");
    let mut c = Command::new("/usr/bin/clang");
    c.arg(&source).arg("-o").arg(&worker);
    let r = process::output(c, Duration::from_secs(30), 1048576)?;
    ensure!(
        r.status.success(),
        "worker compile: {}",
        String::from_utf8_lossy(&r.stderr)
    );
    let mut results = Vec::new();
    for repetition in 1..=repetitions {
        for scope in ["zor-service", "fux-server", "herdr-server"] {
            let mut row = case(scope, &binaries, &worker)?;
            row["repetition"] = json!(repetition);
            results.push(row);
            println!("{scope} {repetition} passed");
            std::io::stdout().flush()?;
        }
    }
    let value = json!({"provenance":provenance,"synthetic":true,"results":results});
    let mut bytes = serde_json::to_vec_pretty(&value)?;
    bytes.push(b'\n');
    fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(output)?
        .write_all(&bytes)?;
    Ok(())
}
