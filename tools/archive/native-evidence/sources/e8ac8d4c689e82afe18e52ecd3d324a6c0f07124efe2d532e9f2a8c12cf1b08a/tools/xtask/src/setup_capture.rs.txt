//! Headless controller setup with synthetic foreground workers and exact diagnostics.
use anyhow::{Context, Result, ensure};
use fux_xtask::support::{
    local::{Root, until},
    process::{self, Guard, OwnedProcess},
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    cell::RefCell,
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
fn decoded(v: &Value) -> Result<Value> {
    Ok(serde_json::from_str(
        v["stdout"].as_str().context("stdout")?,
    )?)
}
fn same_service(second: &Value, state: &Value) -> Result<()> {
    ensure!(
        second
            .get("service_instance")
            .context("dashboard service_instance")?
            == state
                .get("service_instance")
                .context("status service_instance")?,
        "controller was replaced"
    );
    Ok(())
}
fn case(backend: &str, binaries: &BTreeMap<&str, PathBuf>, worker: &Path) -> Result<Value> {
    let mut root = Root::new(
        "setup-pair-rs-",
        &[worker.to_str().context("worker")?.into()],
    )?;
    for (k, p) in [
        ("XDG_CACHE_HOME", "cache"),
        ("WORKER_LOG", "received"),
        ("WORKER_PID", "worker.pid"),
        ("WORKER_RESPONSE", "responded"),
    ] {
        root.env.insert(
            k.into(),
            root.path().join(p).to_str().context("root")?.into(),
        );
    }
    let calls = RefCell::new(Vec::new());
    let mut value = json!({"backend":backend,"startup":[],"configuration":[]});
    let cli = |tool: &str, args: &[&str], purpose: &str, success: bool| -> Result<Value> {
        let mut c = root.command(&binaries[tool]);
        c.args(args);
        let r = process::output(c, Duration::from_secs(12), 512 * 1024)?;
        ensure!(
            r.stdout.len() + r.stderr.len() <= 512 * 1024,
            "CLI output limit"
        );
        let v = json!({"tool":tool,"argv":args,"purpose":purpose,"exit_code":r.status.code(),"stdout":String::from_utf8(r.stdout)?,"stderr":String::from_utf8(r.stderr)?});
        calls.borrow_mut().push(v.clone());
        ensure!(r.status.success() == success, "CLI {v}");
        Ok(v)
    };
    let mut server: Option<Guard> = None;
    let mut zor_started = false;
    let mut worker_pid = None;
    let result = (|| -> Result<()> {
        let (tool, args) = if backend == "zor" {
            cli("zor", &["status"], "unavailable-service diagnostic", false)?;
            let p = root.path().join("config/fux/config.toml");
            value["configuration"]
                .as_array_mut()
                .unwrap()
                .push(json!({"path":p,"contents":fs::read_to_string(&p)?}));
            ("fux", vec!["serve"])
        } else {
            cli(
                "herdr",
                &["agent", "list"],
                "unavailable-service diagnostic",
                false,
            )?;
            for name in ["herdr", "herdr-dev"] {
                let p = root.path().join("config").join(name).join("config.toml");
                fs::create_dir_all(p.parent().context("parent")?)?;
                let contents = format!(
                    "onboarding = false\n[terminal]\ndefault_shell = {}\nshell_mode = \"non_login\"\n[update]\nversion_check = false\nmanifest_check = false\n",
                    serde_json::to_string(worker)?
                );
                fs::write(&p, &contents)?;
                value["configuration"]
                    .as_array_mut()
                    .unwrap()
                    .push(json!({"path":p,"contents":contents}));
            }
            ("herdr", vec!["server"])
        };
        value["startup"]
            .as_array_mut()
            .unwrap()
            .push(json!({"tool":tool,"argv":args}));
        let log = tempfile::tempfile()?;
        let mut c = root.command(&binaries[tool]);
        c.args(args)
            .stdin(Stdio::null())
            .stdout(log.try_clone()?)
            .stderr(log);
        server = Some(Guard(c.spawn()?));
        until(Duration::from_secs(8), || {
            ensure!(
                server.as_mut().unwrap().0.try_wait()?.is_none(),
                "owned server exited"
            );
            let names = if backend == "zor" {
                vec![root.control()]
            } else {
                ["herdr", "herdr-dev"]
                    .map(|n| root.path().join("config").join(n).join("herdr.sock"))
                    .to_vec()
            };
            Ok(names.into_iter().find(|p| p.exists()))
        })?;
        let pane = if backend == "herdr" {
            let created = cli(
                "herdr",
                &[
                    "workspace",
                    "create",
                    "--cwd",
                    root.path().to_str().context("cwd")?,
                    "--focus",
                ],
                "create fixture workspace",
                true,
            )?;
            decoded(&created)?["result"]["root_pane"]["pane_id"].clone()
        } else {
            Value::Null
        };
        worker_pid = Some(until(Duration::from_secs(8), || {
            Ok(fs::read_to_string(root.path().join("worker.pid"))
                .ok()
                .and_then(|s| s.parse::<i32>().ok()))
        })?);
        let pid = worker_pid.unwrap();
        ensure!(pid > 1, "worker pid");
        value["worker_pid"] = json!(pid);
        let overview_args = if backend == "zor" {
            vec!["dashboard", "--once"]
        } else {
            vec!["agent", "list"]
        };
        let purpose = if backend == "zor" {
            "start controller and show overview"
        } else {
            "show overview"
        };
        let mut overview = decoded(&cli(backend, &overview_args, purpose, true)?)?;
        zor_started = backend == "zor";
        let deadline = Instant::now() + Duration::from_secs(8);
        loop {
            let rows = if backend == "zor" {
                &overview["rows"]
            } else {
                &overview["result"]["agents"]
            };
            if !rows.as_array().context("overview rows")?.is_empty() {
                break;
            }
            ensure!(Instant::now() < deadline, "empty overview: {overview}");
            std::thread::sleep(Duration::from_millis(50));
            overview = decoded(&cli(
                backend,
                &overview_args,
                "wait for initial overview",
                true,
            )?)?;
        }
        if backend == "zor" {
            ensure!(
                overview["rows"]
                    .as_array()
                    .context("rows")?
                    .iter()
                    .any(|r| r["kind"] == "observation"
                        && r["label"].as_str().is_some_and(|s| s.contains("claude"))),
                "worker overview"
            );
        } else {
            ensure!(
                overview["result"]["agents"]
                    .as_array()
                    .context("agents")?
                    .iter()
                    .any(|r| r["pane_id"] == pane),
                "worker pane overview"
            );
        }
        value["initial_overview_polls"] = json!(
            calls
                .borrow()
                .iter()
                .filter(|c| c["purpose"] == "wait for initial overview")
                .count()
        );
        value["commands_to_visible_overview"] = json!(
            value["startup"].as_array().unwrap().len()
                + calls
                    .borrow()
                    .iter()
                    .filter(|c| c["purpose"] != "unavailable-service diagnostic")
                    .count()
        );
        if backend == "zor" {
            let state = decoded(&cli(
                "zor",
                &["status"],
                "check shared service identity",
                true,
            )?)?;
            ensure!(
                state["snapshot"]["observations"]
                    .as_array()
                    .context("observations")?
                    .iter()
                    .any(|r| r["detected_pid"] == pid && r["input_sequence"] == 0),
                "identity/input observation"
            );
            let second = decoded(&cli(
                "zor",
                &["dashboard", "--once"],
                "reuse controller and show overview",
                true,
            )?)?;
            same_service(&second, &state)?;
            value["controller_reused"] = json!(true);
            cli("zor", &["shutdown"], "stop only controller", true)?;
            zor_started = false;
            until(Duration::from_secs(8), || {
                Ok((!root.path().join("zor/control.sock").exists()).then_some(()))
            })?;
            ensure!(
                server.as_mut().unwrap().0.try_wait()?.is_none(),
                "multiplexer stopped"
            );
            nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None)?;
            value["worker_survives_controller_stop"] = json!(true);
            cli("zor", &["status"], "stopped-service diagnostic", false)?;
        } else {
            cli(
                "herdr",
                &["agent", "list"],
                "reuse controller and show overview",
                true,
            )?;
            ensure!(
                server.as_mut().unwrap().0.try_wait()?.is_none(),
                "server stopped"
            );
            value["controller_reused"] = json!(true);
            value["worker_survives_controller_stop"] = Value::Null;
            value["controller_stop_scope"] =
                json!("separate agent-controller stop is unsupported; server owns PTYs");
        }
        Ok(())
    })();
    let mut errors = Vec::new();
    if backend == "zor" && (zor_started || root.path().join("zor/control.sock").exists()) {
        let cleanup = (|| -> Result<()> {
            cli("zor", &["shutdown"], "cleanup owned controller", true)?;
            until(Duration::from_secs(8), || {
                Ok((!root.path().join("zor/control.sock").exists()).then_some(()))
            })?;
            Ok(())
        })();
        if let Err(e) = cleanup {
            errors.push(format!("controller cleanup: {e:#}"));
        }
    }
    if let Some(owner) = server.as_mut() {
        let cleanup = (|| -> Result<()> {
            owner.0.terminate()?;
            let status = process::wait(&mut owner.0, Duration::from_secs(10))?;
            ensure!(status.success(), "server exit {status}");
            value["server_exit"] = json!(status.code());
            Ok(())
        })();
        if let Err(e) = cleanup {
            errors.push(format!("server cleanup: {e:#}"));
            if owner.0.try_wait()?.is_none() {
                owner.0.kill()?;
                process::wait(&mut owner.0, Duration::from_secs(5))?;
            }
        }
    }
    if let Some(pid) = worker_pid {
        let gone = until(Duration::from_secs(8), || {
            match nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None) {
                Err(nix::errno::Errno::ESRCH) => Ok(Some(())),
                Ok(()) => Ok(None),
                Err(e) => Err(e.into()),
            }
        });
        if let Err(e) = gone {
            errors.push(format!("worker cleanup: {e:#}"));
        } else {
            value["worker_exit_confirmed"] = json!(true);
        }
    }
    ensure!(errors.is_empty(), "{}", errors.join("; "));
    result?;
    ensure!(
        !root.path().join("received").exists(),
        "setup submitted prompt"
    );
    value["prompt_input_sent"] = json!(false);
    value["calls"] = json!(calls.into_inner());
    let encoded = serde_json::to_string(&value)?
        .replace(
            root.path().canonicalize()?.to_str().context("root")?,
            "<RUN_ROOT>",
        )
        .replace(root.path().to_str().context("root")?, "<RUN_ROOT>");
    let value = serde_json::from_str(&encoded)?;
    crate::evidence::validate_setup_case(&value)?;
    Ok(value)
}
pub fn run(args: Vec<String>) -> Result<()> {
    ensure!(
        args.len().is_multiple_of(2),
        "usage: capture-controller-setup --fux PATH --zor PATH --herdr PATH --herdr-provenance JSON --output NEW_JSON [--repetitions 1..5]"
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
    ensure!((1..=5).contains(&repetitions), "repetition bounds");
    let output = Path::new(flags.get("--output").context("output")?);
    ensure!(!output.exists(), "use new output");
    let mut binaries = BTreeMap::new();
    let mut provenance = json!({"harness_kind":"rust-controller-setup","worker_sha256":format!("{:x}",Sha256::digest(include_bytes!("setup-worker.c"))),"binaries":{},"source_hashes":{}});
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .context("root")?;
    provenance["harness_sha256"] =
        digest(&repository.join("tools/xtask/src/setup_capture.rs"))?.into();
    for name in ["fux", "zor", "herdr"] {
        let key = format!("--{name}");
        let p = Path::new(flags.get(key.as_str()).context("binary")?).canonicalize()?;
        provenance["binaries"][name] = json!({"path":p,"sha256":digest(&p)?});
        binaries.insert(name, p);
    }
    let build: Value = serde_json::from_slice(&fs::read(
        flags.get("--herdr-provenance").context("build")?,
    )?)?;
    ensure!(
        build["binary_sha256"] == provenance["binaries"]["herdr"]["sha256"],
        "herdr provenance mismatch"
    );
    provenance["herdr_build"] = build;
    for p in [
        "zor/src/service.rs",
        "zor/src/dashboard.rs",
        "src/config.rs",
        "src/proto/control.rs",
    ] {
        provenance["source_hashes"][p] = digest(&repository.join(p))?.into();
    }
    let temp = tempfile::Builder::new()
        .prefix("setup-worker-rs-")
        .tempdir_in("/tmp")?;
    for (name, binary) in &binaries {
        let mut c = Command::new(binary);
        c.arg("--version")
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", temp.path())
            .env("XDG_CONFIG_HOME", temp.path().join("config"));
        let r = process::output(c, Duration::from_secs(5), 65536)?;
        ensure!(r.status.success(), "version failed");
        provenance["binaries"][name]["version"] = String::from_utf8(r.stdout)?.trim().into();
    }
    ensure!(
        provenance["binaries"]["herdr"]["version"] == "herdr 0.8.2",
        "reference version"
    );
    let source = temp.path().join("worker.c");
    fs::write(&source, include_bytes!("setup-worker.c"))?;
    let worker = temp.path().join("claude");
    let mut c = Command::new("/usr/bin/clang");
    c.arg(&source).arg("-o").arg(&worker);
    let r = process::output(c, Duration::from_secs(30), 1024 * 1024)?;
    ensure!(
        r.status.success(),
        "worker compile: {}",
        String::from_utf8_lossy(&r.stderr)
    );
    let mut results = Vec::new();
    for repetition in 1..=repetitions {
        for backend in ["zor", "herdr"] {
            let mut row = case(backend, &binaries, &worker)?;
            row["repetition"] = json!(repetition);
            results.push(row);
            println!("{backend} {repetition} passed");
            std::io::stdout().flush()?;
        }
    }
    let v = json!({"schema":1,"captured_at":time::OffsetDateTime::now_utc().format(&time::format_description::well_known::Rfc3339)?,"provenance":provenance,"synthetic":true,"results":results,"limitation":"Prepared installed binaries and fixture configs; headless setup path only, not installation/onboarding, agent accuracy or a universal usability score."});
    let encoded = (serde_json::to_string_pretty(&v)? + "\n")
        .replace(
            temp.path()
                .canonicalize()?
                .to_str()
                .context("worker root")?,
            "<WORKER_ROOT>",
        )
        .replace(
            temp.path().to_str().context("worker root")?,
            "<WORKER_ROOT>",
        );
    if let Some(p) = output.parent().filter(|p| !p.as_os_str().is_empty()) {
        fs::create_dir_all(p)?;
    }
    fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(output)?
        .write_all(encoded.as_bytes())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reuse_requires_both_service_identities() {
        let valid = json!({"service_instance":"a"});
        assert!(same_service(&valid, &valid).is_ok());
        for (dashboard, status) in [
            (json!({}), json!({})),
            (json!({}), valid.clone()),
            (valid.clone(), json!({})),
            (valid, json!({"service_instance":"b"})),
        ] {
            assert!(same_service(&dashboard, &status).is_err());
        }
    }
}
