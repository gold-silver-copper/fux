//! Real signed-out Codex startup and pane loss; no prompt or credentials.
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
    process::Stdio,
    time::{Duration, Instant},
};
fn digest(path: &Path) -> Result<String> {
    Ok(format!("{:x}", Sha256::digest(fs::read(path)?)))
}
fn cli(root: &Root, binary: &Path, args: &[&str]) -> Result<Value> {
    let mut c = root.command(binary);
    c.args(args);
    let r = process::output(c, Duration::from_secs(5), 1024 * 1024)?;
    ensure!(
        r.status.success(),
        "CLI {args:?}: {}",
        String::from_utf8_lossy(&r.stderr)
    );
    Ok(serde_json::from_slice(&r.stdout)?)
}
struct Backend {
    zor: bool,
    endpoint: PathBuf,
    pane: Value,
}
impl Backend {
    fn call(&self, method: &str, params: Value) -> Result<Value> {
        let r = service::rpc(
            &self.endpoint,
            &json!({"id":"freshness","method":method,"params":params}),
            Duration::from_secs(3),
        )?;
        ensure!(r.get("error").is_none(), "herdr {method}: {r}");
        Ok(r["result"].clone())
    }
    fn capture(&self) -> Result<Value> {
        if self.zor {
            completed(
                &self.endpoint,
                json!({"id":1,"command":"capture","pane":self.pane,"max_bytes":131072}),
            )
        } else {
            self.call(
                "pane.read",
                json!({"pane_id":self.pane,"source":"visible","strip_ansi":true}),
            )
        }
    }
    fn observe(&self, root: &Root, binary: &Path) -> Result<Value> {
        if self.zor {
            Ok(cli(root, binary, &["status"])?["snapshot"].clone())
        } else {
            self.call("agent.list", json!({}))
        }
    }
    fn agent<'a>(&self, v: &'a Value) -> Result<Option<&'a Value>> {
        let rows = v[if self.zor { "observations" } else { "agents" }]
            .as_array()
            .context("agent rows")?;
        Ok(rows.iter().find(|r| {
            if self.zor {
                r.pointer("/handle/pane") == Some(&self.pane)
            } else {
                r.get("pane_id") == Some(&self.pane)
            }
        }))
    }
    fn state(&self, v: &Value) -> Result<Value> {
        Ok(self
            .agent(v)?
            .and_then(|r| r.get(if self.zor { "state" } else { "agent_status" }))
            .cloned()
            .unwrap_or(Value::Null))
    }
    fn close(&self) -> Result<()> {
        if self.zor {
            completed(
                &self.endpoint,
                json!({"id":1,"command":"kill","pane":self.pane}),
            )?;
        } else {
            self.call("pane.close", json!({"pane_id":self.pane}))?;
        }
        Ok(())
    }
}
fn case(name: &str, binaries: &BTreeMap<&str, PathBuf>) -> Result<Value> {
    let mut root = Root::new(
        "freshness-rs-",
        &[binaries["codex"].to_str().context("codex path")?.into()],
    )?;
    fs::create_dir(root.path().join("codex"))?;
    for (k, v) in [
        ("PATH", "/usr/bin:/bin:/usr/sbin:/sbin:/opt/homebrew/bin"),
        ("LANG", "en_US.UTF-8"),
    ] {
        root.env.insert(k.into(), v.into());
    }
    root.env.insert(
        "XDG_CACHE_HOME".into(),
        root.path().join("cache").to_str().context("cache")?.into(),
    );
    // Only this disposable child environment points Codex at the empty account directory.
    root.env.insert(
        "CODEX_HOME".into(),
        root.path()
            .join("codex")
            .to_str()
            .context("codex home")?
            .into(),
    );
    let log = tempfile::tempfile()?;
    let mut owners: Vec<(&str, Guard)> = Vec::new();
    let mut agent_pid = None;
    let result = (|| -> Result<Value> {
        let mut start = |name: &'static str, args: &[&str]| -> Result<()> {
            let mut c = root.command(&binaries[name]);
            c.args(args)
                .stdin(Stdio::null())
                .stdout(log.try_clone()?)
                .stderr(log.try_clone()?);
            owners.push((name, Guard(c.spawn()?)));
            Ok(())
        };
        let backend = if name == "zor" {
            start("fux", &["serve"])?;
            let endpoint = root.control();
            until(Duration::from_secs(8), || {
                Ok(endpoint.exists().then_some(()))
            })?;
            let listing = completed(&endpoint, json!({"id":1,"command":"list"}))?;
            let pane = listing
                .pointer("/workspaces/0/tabs/0/panes/0/id")
                .context("pane")?
                .clone();
            start("zor", &["serve"])?;
            until(Duration::from_secs(8), || {
                Ok(root.path().join("zor/control.sock").exists().then_some(()))
            })?;
            Backend {
                zor: true,
                endpoint,
                pane,
            }
        } else {
            for name in ["herdr", "herdr-dev"] {
                let p = root.path().join("config").join(name).join("config.toml");
                fs::create_dir_all(p.parent().context("config parent")?)?;
                fs::write(
                    p,
                    format!(
                        "onboarding = false\n[terminal]\ndefault_shell = {}\nshell_mode = \"non_login\"\n[update]\nversion_check = false\nmanifest_check = false\n",
                        serde_json::to_string(&binaries["codex"])?
                    ),
                )?;
            }
            start("herdr", &["server"])?;
            let endpoint = until(Duration::from_secs(8), || {
                for n in ["herdr", "herdr-dev"] {
                    let p = root.path().join("config").join(n).join("herdr.sock");
                    if p.exists() {
                        return Ok(Some(p));
                    }
                }
                Ok(None)
            })?;
            let mut b = Backend {
                zor: false,
                endpoint,
                pane: Value::Null,
            };
            b.pane=b.call("workspace.create",json!({"cwd":root.path(),"focus":true}))?["root_pane"]["pane_id"].clone();
            b
        };
        let blocker = |v: &Value| {
            let text = v.to_string();
            text.contains("Sign in with ChatGPT") && text.contains("Press enter to continue")
        };
        let visible = until(Duration::from_secs(12), || {
            let v = backend.capture()?;
            Ok(blocker(&v).then_some(v))
        })?;
        let started = Instant::now();
        let mut first = None;
        let mut samples = Vec::new();
        while started.elapsed() < Duration::from_secs(3) {
            let observation = backend.observe(&root, &binaries["zor"])?;
            let state = backend.state(&observation)?;
            let elapsed = started.elapsed().as_secs_f64() * 1000.;
            if state == "blocked" && first.is_none() {
                first = Some(elapsed)
            }
            if name == "zor"
                && let Some(row) = backend.agent(&observation)?
            {
                agent_pid = row
                    .get("detected_pid")
                    .and_then(Value::as_i64)
                    .map(i32::try_from)
                    .transpose()?;
            }
            samples.push(json!({"elapsed_ms":elapsed,"observation":observation,"state":state}));
            std::thread::sleep(Duration::from_millis(50));
        }
        ensure!(
            samples.iter().any(|s| !s["state"].is_null()),
            "agent never discovered"
        );
        let final_capture = backend.capture()?;
        ensure!(blocker(&final_capture), "blocker no longer visible");
        if name == "herdr" {
            agent_pid = Some(i32::try_from(
                backend.call("pane.process_info", json!({"pane_id":backend.pane}))?["process_info"]
                    ["shell_pid"]
                    .as_i64()
                    .context("shell PID")?,
            )?);
        }
        ensure!(agent_pid.is_some_and(|p| p > 1), "missing agent PID");
        let close_started = Instant::now();
        backend.close()?;
        let removed = until(Duration::from_secs(8), || {
            let v = backend.observe(&root, &binaries["zor"])?;
            Ok(backend.agent(&v)?.is_none().then_some(v))
        })?;
        let removed_ms = close_started.elapsed().as_secs_f64() * 1000.;
        Ok(
            json!({"backend":name,"expected":"blocked","visible_capture":visible,"samples":samples,"first_blocked_ms":first,"observation_window_ms":close_started.duration_since(started).as_secs_f64()*1000.,"close_to_absence_ms":removed_ms,"after_close":removed,"prompt_input_sent":false,"final_capture":final_capture,"agent_pid":agent_pid}),
        )
    })();
    let mut errors = Vec::new();
    for (name, owner) in owners.iter_mut().rev() {
        if *name == "zor" && owner.0.try_wait()?.is_none() {
            if let Err(e) = cli(&root, &binaries["zor"], &["shutdown"]) {
                errors.push(format!("zor shutdown: {e:#}"));
            }
        } else if owner.0.try_wait()?.is_none()
            && let Err(e) = owner.0.terminate()
        {
            errors.push(format!("{name} terminate: {e:#}"));
        }
        match process::wait(&mut owner.0, Duration::from_secs(10)) {
            Ok(s) => {
                if !s.success() {
                    errors.push(format!("{name}: {s}"));
                }
            }
            Err(e) => {
                errors.push(format!("{name}: {e:#}"));
                if let Err(e) = owner.0.kill().map(|_| ()) {
                    errors.push(e.to_string());
                }
                if let Err(e) = process::wait(&mut owner.0, Duration::from_secs(5)) {
                    errors.push(e.to_string());
                }
            }
        }
    }
    if let Some(pid) = agent_pid
        && let Err(e) = until(Duration::from_secs(8), || {
            match nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None) {
                Err(nix::errno::Errno::ESRCH) => Ok(Some(())),
                Ok(()) => Ok(None),
                Err(e) => Err(e.into()),
            }
        })
    {
        errors.push(format!("agent cleanup: {e:#}"));
    }
    ensure!(errors.is_empty(), "{}", errors.join("; "));
    let mut result = result?;
    let mut exits = serde_json::Map::new();
    for (name, owner) in &mut owners {
        exits.insert(
            (*name).into(),
            json!(owner.0.try_wait()?.context("owner still running")?.code()),
        );
    }
    result["owners_exit"] = Value::Object(exits);
    result["agent_pid_exit_checked"] = json!(agent_pid.is_some());
    let text = serde_json::to_string(&result)?
        .replace(
            root.path().canonicalize()?.to_str().context("root")?,
            "<RUN_ROOT>",
        )
        .replace(root.path().to_str().context("root")?, "<RUN_ROOT>");
    Ok(serde_json::from_str(&text)?)
}
pub fn run(args: Vec<String>) -> Result<()> {
    ensure!(
        args.len().is_multiple_of(2),
        "usage: capture-detection-freshness --fux PATH --zor PATH --herdr PATH --codex PATH --herdr-provenance JSON --output NEW_JSON [--repetitions 1..3]"
    );
    let mut flags = BTreeMap::new();
    for p in args.chunks_exact(2) {
        ensure!(
            [
                "--fux",
                "--zor",
                "--herdr",
                "--codex",
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
    ensure!((1..=3).contains(&repetitions), "repetitions must be1..3");
    let output = Path::new(flags.get("--output").context("missing output")?);
    ensure!(!output.exists(), "use new output file");
    let mut binaries = BTreeMap::new();
    let mut provenance = json!({"harness_kind":"rust-signed-out-freshness","binaries":{},"sources":{},"herdr_build":serde_json::from_slice::<Value>(&fs::read(flags.get("--herdr-provenance").context("missing provenance")?)?)?});
    for name in ["fux", "zor", "herdr", "codex"] {
        let flag = format!("--{name}");
        let p = std::path::absolute(flags.get(flag.as_str()).context("missing binary")?)?;
        provenance["binaries"][name] = json!({"path":p,"sha256":digest(&p)?});
        binaries.insert(name, p);
    }
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .context("root")?;
    for p in [
        "tools/xtask/src/freshness_capture.rs",
        "zor/src/watch.rs",
        "zor/rules/codex.toml",
    ] {
        provenance["sources"][p] = digest(&root.join(p))?.into();
    }
    ensure!(
        provenance["herdr_build"]["binary_sha256"] == provenance["binaries"]["herdr"]["sha256"],
        "herdr provenance mismatch"
    );
    let mut results = Vec::new();
    for repetition in 1..=repetitions {
        for backend in ["zor", "herdr"] {
            let mut row = case(backend, &binaries)?;
            row["repetition"] = json!(repetition);
            results.push(row);
            println!("{backend} {repetition} passed");
            std::io::stdout().flush()?;
        }
    }
    let v = json!({"recorded_at":time::OffsetDateTime::now_utc().format(&time::format_description::well_known::Rfc3339)?,"provenance":provenance,"results":results});
    let mut bytes = serde_json::to_vec_pretty(&v)?;
    bytes.push(b'\n');
    fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(output)?
        .write_all(&bytes)?;
    Ok(())
}
