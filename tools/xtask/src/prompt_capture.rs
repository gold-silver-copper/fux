//! Synthetic prompt-boundary contracts; no real provider detection or correctness claim.
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
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::Duration,
};
pub fn digest(p: &Path) -> Result<String> {
    Ok(format!("{:x}", Sha256::digest(fs::read(p)?)))
}
pub struct Harness {
    // Stop the owned server before dropping its private directory, including on failure.
    pub server: Guard,
    pub log: fs::File,
    pub root: Root,
    pub control: PathBuf,
}
impl Harness {
    pub fn start(backend: &str, binary: &Path, worker: &Path) -> Result<Self> {
        let mut root = Root::new(
            "prompt-pair-rs-",
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
        if backend == "herdr" {
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
        let log = tempfile::tempfile()?;
        let mut c = root.command(binary);
        c.arg(if backend == "herdr" {
            "server"
        } else {
            "serve"
        })
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log.try_clone()?);
        let server = Guard(c.spawn()?);
        let mut h = Self {
            server,
            log,
            root,
            control: PathBuf::new(),
        };
        h.control = until(Duration::from_secs(8), || {
            ensure!(
                h.server.0.try_wait()?.is_none(),
                "server exited before readiness"
            );
            let paths = if backend == "herdr" {
                ["herdr", "herdr-dev"]
                    .map(|n| h.root.path().join("config").join(n).join("herdr.sock"))
                    .to_vec()
            } else {
                vec![h.root.control()]
            };
            Ok(paths.into_iter().find(|p| p.exists()))
        })?;
        Ok(h)
    }
    pub fn call(&self, method: &str, params: Value) -> Result<Value> {
        service::rpc(
            &self.control,
            &json!({"id":"comparison","method":method,"params":params}),
            Duration::from_secs(10),
        )
    }
    pub fn task(&self, zor: &Path, args: &[&str], ok: bool) -> Result<Value> {
        let mut c = self.root.command(zor);
        c.arg("task").args(args);
        let r = process::output(c, Duration::from_secs(10), 1048576)?;
        ensure!(
            r.status.success() == ok,
            "task {args:?}: stdout={} stderr={}",
            String::from_utf8_lossy(&r.stdout),
            String::from_utf8_lossy(&r.stderr)
        );
        if ok {
            Ok(serde_json::from_slice(&r.stdout)?)
        } else {
            Ok(
                json!({"exit":r.status.code(),"error":String::from_utf8(r.stderr)?.chars().take(4096).collect::<String>()}),
            )
        }
    }
    pub fn received(&self) -> Result<String> {
        let p = self.root.path().join("received");
        if p.exists() {
            Ok(fs::read_to_string(p)?)
        } else {
            Ok(String::new())
        }
    }
    pub fn diagnostic(&mut self) -> Result<()> {
        self.log.seek(SeekFrom::Start(0))?;
        let mut bytes = Vec::new();
        (&mut self.log).take(16384).read_to_end(&mut bytes)?;
        eprintln!("{}", String::from_utf8_lossy(&bytes));
        Ok(())
    }
    pub fn finish(&mut self) -> Result<i32> {
        if self.server.0.try_wait()?.is_none() {
            self.server.0.terminate()?;
        }
        let status = match process::wait(&mut self.server.0, Duration::from_secs(10)) {
            Ok(s) => s,
            Err(e) => {
                self.server.0.kill()?;
                process::wait(&mut self.server.0, Duration::from_secs(3))?;
                return Err(e.context("forced termination; worker cleanup unproven"));
            }
        };
        ensure!(status.success(), "server cleanup: {status}");
        let pid = fs::read_to_string(self.root.path().join("worker.pid"))?.parse::<i32>()?;
        ensure!(pid > 1, "worker PID");
        until(Duration::from_secs(8), || {
            match nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None) {
                Err(nix::errno::Errno::ESRCH) => Ok(Some(())),
                Ok(()) => Ok(None),
                Err(e) => Err(e.into()),
            }
        })?;
        status.code().context("server signal exit")
    }
}
fn report(
    h: &Harness,
    pane: &Value,
    state: &str,
    seq: &mut u64,
    reports: &mut Vec<Value>,
) -> Result<Value> {
    *seq += 1;
    let r=h.call("pane.report_agent",json!({"pane_id":pane,"source":"comparison:fixture","agent":"claude","state":state,"seq":seq}))?;
    ensure!(r.get("error").is_none(), "report: {r}");
    let observed = until(Duration::from_secs(8), || {
        let v = h.call("agent.get", json!({"target":pane}))?;
        Ok((v.pointer("/result/agent/agent_status") == Some(&json!(state))).then_some(v))
    })?;
    reports.push(json!({"source_sequence":seq,"state":state,"state_change_seq":observed.pointer("/result/agent/state_change_seq").context("state sequence")?}));
    Ok(observed)
}
fn herdr_case(h: &Harness, case: &str) -> Result<Value> {
    let created = h.call(
        "workspace.create",
        json!({"cwd":h.root.path(),"focus":true}),
    )?;
    ensure!(created.get("error").is_none(), "workspace: {created}");
    let listing = h.call("pane.list", json!({}))?;
    let panes = listing
        .pointer("/result/panes")
        .and_then(Value::as_array)
        .context("panes")?;
    ensure!(panes.len() == 1, "one pane");
    let pane = panes[0].get("pane_id").context("pane ID")?;
    let mut seq = 0;
    let mut reports = Vec::new();
    let before = report(
        h,
        pane,
        if case == "preblocked" {
            "blocked"
        } else {
            "idle"
        },
        &mut seq,
        &mut reports,
    )?;
    let text = if matches!(case, "stale-idle" | "preblocked") {
        "silent"
    } else {
        "respond"
    };
    // Dispatch without receiving so separate hook requests can run concurrently.
    let pending = service::Peer::send(
        &h.control,
        &json!({"id":"comparison","method":"agent.prompt","params":{"target":pane,"text":text,"wait":{"until":["idle","blocked"],"timeout_ms":1500}}}),
        Duration::from_secs(10),
    )?;
    if matches!(case, "immediate-response" | "working-response") {
        until(Duration::from_secs(8), || {
            Ok(h.root.path().join("responded").exists().then_some(()))
        })?;
        if case == "working-response" {
            report(h, pane, "working", &mut seq, &mut reports)?;
            std::thread::sleep(Duration::from_millis(150));
        }
        report(h, pane, "idle", &mut seq, &mut reports)?;
    }
    let response = pending.receive()?;
    let received = h.received()?;
    ensure!(
        received
            == if case == "preblocked" {
                String::new()
            } else {
                format!("{text}\n")
            },
        "unexpected input: {received:?}"
    );
    if case == "working-response" {
        ensure!(
            response.pointer("/result/type") == Some(&json!("agent_prompted")),
            "working response: {response}"
        );
    } else {
        ensure!(
            response.pointer("/error/code")
                == Some(&json!(if case == "preblocked" {
                    "agent_blocked"
                } else {
                    "timeout"
                })),
            "baseline changed: {response}"
        );
    }
    Ok(json!({"before":before,"response":response,"received":received,"reports":reports}))
}
fn zor_case(h: &Harness, case: &str, zor: &Path) -> Result<Value> {
    macro_rules! t {($($a:expr),* $(,)?)=>{h.task(zor,&[$($a),*],true)?};}
    let listing = completed(&h.control, json!({"id":1,"command":"list"}))?;
    let pane = listing
        .pointer("/workspaces/0/tabs/0/panes/0/id")
        .context("pane")?;
    t!(
        "adopt",
        "worker",
        "--title",
        "comparison",
        "--instance",
        listing["instance"].as_str().context("instance")?,
        "--workspace",
        "default",
        "--pane",
        &pane.to_string()
    );
    let delivered = |op: &str| {
        until(Duration::from_secs(8), || {
            let v = t!("reconcile", op);
            Ok((v["delivery"] == "delivered").then_some(v))
        })
    };
    let mut prior_wait = Value::Null;
    let mut prior_input = "";
    if case == "stale-idle" {
        let old = t!(
            "prepare",
            "worker",
            "--operation",
            "previous",
            "--text",
            "seed",
            "--timeout-ms",
            "5000"
        );
        t!("submit", "previous");
        let receipt = delivered("previous")?;
        until(Duration::from_secs(8), || {
            Ok(h.root.path().join("responded").exists().then_some(()))
        })?;
        t!(
            "report",
            "previous",
            "--token",
            old["report_token"].as_str().context("token")?,
            "--producer",
            "fixture-1",
            "--sequence",
            "1",
            "--input-operation",
            &receipt["receipt"]["operation"].to_string(),
            "--kind",
            "response-observed"
        );
        prior_wait = t!("wait", "previous")["wait"].clone();
        ensure!(prior_wait == "response-observed", "prior prompt");
        prior_input = "seed\n";
    }
    let text = if case == "stale-idle" {
        "silent"
    } else {
        "respond"
    };
    let prepared = t!(
        "prepare",
        "worker",
        "--operation",
        "prompt",
        "--text",
        text,
        "--timeout-ms",
        "1500"
    );
    t!("submit", "prompt");
    let receipt = delivered("prompt")?;
    until(Duration::from_secs(8), || {
        Ok((h.received()? == format!("{prior_input}{text}\n")).then_some(()))
    })?;
    if matches!(case, "immediate-response" | "working-response") {
        until(Duration::from_secs(8), || {
            Ok(h.root.path().join("responded").exists().then_some(()))
        })?;
        t!(
            "report",
            "prompt",
            "--token",
            prepared["report_token"].as_str().context("token")?,
            "--producer",
            "fixture-1",
            "--sequence",
            "1",
            "--input-operation",
            &receipt["receipt"]["operation"].to_string(),
            "--kind",
            "response-observed"
        );
    }
    let response = t!("wait", "prompt", "--follow", "--timeout-ms", "3000")["prompt"].clone();
    ensure!(
        response["wait"]
            == if case == "stale-idle" {
                "timed-out"
            } else {
                "response-observed"
            },
        "prompt wait: {response}"
    );
    let received = h.received()?;
    ensure!(
        received == format!("{prior_input}{text}\n"),
        "unexpected input"
    );
    Ok(
        json!({"response":{"delivery":response.get("delivery").context("delivery")?,"wait":response["wait"]},"task_outcome":t!("inspect","worker")["task"]["outcome"],"prior_prompt_wait":prior_wait,"received":received.strip_prefix(prior_input).context("prior input")?}),
    )
}
fn case(
    backend: &str,
    case: &str,
    binaries: &BTreeMap<&str, PathBuf>,
    worker: &Path,
) -> Result<Value> {
    let mut h = Harness::start(
        backend,
        &binaries[if backend == "herdr" { "herdr" } else { "fux" }],
        worker,
    )?;
    let measured = if backend == "herdr" {
        herdr_case(&h, case)
    } else {
        zor_case(&h, case, &binaries["zor"])
    };
    if measured.is_err() {
        h.diagnostic()?;
    }
    let cleanup = h.finish();
    if let Err(e) = &cleanup {
        eprintln!("cleanup: {e:#}");
    }
    let mut value = measured?;
    let exit = cleanup?;
    value["worker_exit_confirmed"] = json!(true);
    value["backend"] = json!(backend);
    value["case"] = json!(case);
    value["server_exit"] = json!(exit);
    Ok(value)
}
pub fn run(args: Vec<String>) -> Result<()> {
    ensure!(
        args.len().is_multiple_of(2),
        "usage: capture-prompt-boundary --herdr PATH --fux PATH --zor PATH --output NEW_JSON [--repetitions 1..20] [--herdr-reference DIR]"
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
    let mut binaries = BTreeMap::new();
    let mut provenance = json!({"harness_kind":"rust-prompt-boundary","sources":{}});
    let root = Root::new("prompt-worker-rs-", &["/bin/cat".into()])?;
    for name in ["herdr", "fux", "zor"] {
        let key = format!("--{name}");
        let p = Path::new(flags.get(key.as_str()).context("binary")?).canonicalize()?;
        let mut c = root.command(&p);
        c.arg("--version");
        let r = process::output(c, Duration::from_secs(5), 65536)?;
        ensure!(r.status.success(), "version failed");
        provenance[name] =
            json!({"sha256":digest(&p)?,"version":String::from_utf8(r.stdout)?.trim()});
        binaries.insert(name, p);
    }
    ensure!(
        provenance["herdr"]["version"]
            .as_str()
            .context("herdr version")?
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
    ensure!(r.status.success(), "reference revision");
    provenance["reference_commit"] = json!(String::from_utf8(r.stdout)?.trim());
    let mut c = Command::new("/usr/bin/uname");
    c.arg("-a");
    let r = process::output(c, Duration::from_secs(5), 65536)?;
    ensure!(r.status.success(), "platform");
    provenance["platform"] = json!(String::from_utf8(r.stdout)?.trim());
    provenance["worker_source_sha256"] = json!(format!(
        "{:x}",
        Sha256::digest(include_bytes!("prompt-worker.c"))
    ));
    provenance["harness_sha256"] = json!(digest(
        &repository.join("tools/xtask/src/prompt_capture.rs")
    )?);
    for p in [
        "tools/xtask/src/prompt_capture.rs",
        "tools/xtask/src/prompt-worker.c",
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
        for name in [
            "stale-idle",
            "immediate-response",
            "working-response",
            "preblocked",
        ] {
            for backend in if name == "preblocked" {
                vec!["herdr"]
            } else {
                vec!["herdr", "zor"]
            } {
                let mut v = case(backend, name, &binaries, &worker)?;
                v["repetition"] = json!(repetition);
                results.push(v);
                println!("{backend} {name} {repetition} passed");
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
