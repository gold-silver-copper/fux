//! Real explicit zor task resume with retained OpenCode session and lost creation reply.
use crate::{
    capture_native::{events, normalized, until},
    native_provider::Provider,
    resume,
    resume_proxy::Proxy,
    runtime::{self, Owner, Root},
};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs,
    os::unix::fs::DirBuilderExt,
    path::Path,
    process::Stdio,
    time::{Duration, Instant},
};
const PROMPTS: [&str; 2] = [
    "RESUME_FIRST: reply briefly",
    "RESUME_SECOND: reply briefly",
];
fn stop(process: &mut Option<Owner>, evidence: &mut Value) -> Result<()> {
    if let Some(mut owner) = process.take() {
        let status = owner.stop()?;
        evidence["exits"]
            .as_array_mut()
            .context("exits")?
            .push(json!(status.code()));
        ensure!(status.success(), "owned fux exit: {status}");
    }
    Ok(())
}
pub fn run(args: Vec<String>) -> Result<()> {
    ensure!(
        args.len() == 8,
        "usage: capture-zor-resume --zor PATH --fux PATH --opencode PATH --output NEW_DIRECTORY"
    );
    let mut flags = BTreeMap::new();
    for p in args.as_chunks::<2>().0 {
        ensure!(
            ["--fux", "--zor", "--opencode", "--output"].contains(&p[0].as_str()),
            "unknown flag"
        );
        ensure!(
            flags.insert(p[0].as_str(), p[1].as_str()).is_none(),
            "duplicate flag"
        );
    }
    let fux = Path::new(flags.get("--fux").context("fux")?).canonicalize()?;
    let agent = Path::new(flags.get("--opencode").context("opencode")?).canonicalize()?;
    let zor = Path::new(flags.get("--zor").context("zor")?).canonicalize()?;
    let output = Path::new(flags.get("--output").context("output")?);
    ensure!(!output.exists(), "use new output directory");
    if let Some(p) = output.parent().filter(|p| !p.as_os_str().is_empty()) {
        fs::create_dir_all(p)?;
    }
    fs::DirBuilder::new().mode(0o700).create(output)?;
    let mut root = Root::new("zresume-rs-")?;
    let path = root.path().canonicalize()?;
    let plugin = path.join("probe.mjs");
    fs::write(&plugin, include_bytes!("native-probe.mjs"))?;
    let mut provider = Provider::start_resume()?;
    let config = json!({"plugin":[format!("file://{}",plugin.display())],"model":"fixture/fixture","small_model":"fixture/fixture","enabled_providers":["fixture"],"permission":{"*":"deny"},"provider":{"fixture":{"npm":"@ai-sdk/openai-compatible","name":"Local Fixture","options":{"baseURL":format!("http://127.0.0.1:{}/v1",provider.port),"apiKey":"fixture"},"models":{"fixture":{"name":"Fixture","limit":{"context":65536,"output":1024}}}}}});
    let config_path = path.join("work/opencode.json");
    fs::write(&config_path, serde_json::to_vec(&config)?)?;
    for (key, value) in [
        ("PATH", "/usr/bin:/bin:/usr/sbin:/sbin".into()),
        ("TMPDIR", path.to_str().context("root")?.into()),
        (
            "OPENCODE_CONFIG",
            config_path.to_str().context("config")?.into(),
        ),
        (
            "ZOR_PROBE_LOG",
            path.join("events.jsonl").to_str().context("events")?.into(),
        ),
        ("OPENCODE_DISABLE_DEFAULT_PLUGINS", "true".into()),
        ("OPENCODE_DISABLE_MODELS_FETCH", "true".into()),
        ("OPENCODE_DISABLE_AUTOUPDATE", "true".into()),
    ] {
        root.set_env(key, value);
    }
    let mut evidence = json!({"fux_sha256":runtime::hash(&fux)?,"opencode_sha256":runtime::hash(&agent)?,"harness_kind":"rust-binary","harness_sha256":runtime::hash(&std::env::current_exe()?)?,"requests":[],"captures":[],"targets":[],"submissions":[],"exits":[],"limitation":"Real OpenCode TUI and persisted session, synthetic provider; explicit zor resume policy; no model-quality claim."});
    evidence["zor_sha256"] = runtime::hash(&zor)?.into();
    evidence["zor_attempts"] = json!([]);
    evidence["zor_outcomes"] = json!([]);
    let journal = path.join("state/zor/journal.json");
    let task = |args: &[&str], ok: bool| -> Result<Value> {
        let deadline = Instant::now() + Duration::from_secs(12);
        loop {
            let left = deadline
                .checked_duration_since(Instant::now())
                .context("task deadline")?;
            let mut c = root.command(&zor);
            c.arg("task").args(args);
            let r = runtime::output(c, left)?;
            if r.stderr == b"zor: zor journal is busy; retry the operation ID\n"
                && Instant::now() < deadline
            {
                std::thread::sleep(Duration::from_millis(30));
                continue;
            }
            ensure!(
                r.status.success() == ok,
                "task {args:?}: {}",
                String::from_utf8_lossy(&r.stderr)
            );
            return if ok {
                Ok(serde_json::from_slice(&r.stdout)?)
            } else {
                Ok(json!(String::from_utf8(r.stderr)?))
            };
        }
    };
    let read_journal = || -> Result<Value> { Ok(serde_json::from_slice(&fs::read(&journal)?)?) };
    let refusal = |args: &[&str], expected: &str| -> Result<()> {
        let before = fs::read(&journal)?;
        let error = task(args, false)?;
        ensure!(
            error.as_str().context("refusal")?.contains(expected),
            "refusal: {error}"
        );
        ensure!(fs::read(&journal)? == before, "refusal mutated journal");
        Ok(())
    };
    let mut proxy: Option<Proxy> = None;
    let control = path.join("fux/default.sock");
    let mut process = None;
    let outcome = (|| -> Result<()> {
        for args in [
            vec!["init", "-b", "main"],
            vec!["config", "user.name", "Fixture"],
            vec!["config", "user.email", "fixture@example.invalid"],
            vec!["add", "opencode.json"],
            vec!["commit", "-m", "fixture"],
        ] {
            let mut c = root.command(Path::new("/usr/bin/git"));
            c.arg("-C").arg(path.join("work")).args(args);
            ensure!(
                runtime::output(c, Duration::from_secs(5))?.status.success(),
                "fixture git"
            );
        }

        for (arg, key) in [("--version", "version"), ("--help", "help")] {
            let mut c = root.command(&agent);
            c.arg(arg);
            let r = runtime::output(c, Duration::from_secs(10))?;
            ensure!(r.status.success(), "OpenCode {arg}");
            let text = String::from_utf8(r.stdout)?;
            evidence[key] = json!(if key == "version" {
                text.trim().to_owned()
            } else {
                text
            });
        }
        let mut session: Option<Value> = None;
        for (turn, prompt) in PROMPTS.iter().enumerate() {
            let argv = [
                agent.to_str().context("agent")?,
                "--model",
                "fixture/fixture",
            ];
            fs::write(
                path.join("config/fux/config.toml"),
                "default-command = { argv = [\"/bin/cat\"] }\n",
            )?;
            let mut c = root.command(&fux);
            c.arg("serve")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(fs::File::create(path.join(format!("stderr-{turn}")))?);
            process = Some(Owner(c.spawn()?));
            let owner = process.as_mut().unwrap();
            until(owner, || Ok(control.exists().then_some(())))?;
            let listing = runtime::rpc(&control, json!({"id":1,"command":"list"}))?;
            let instance = listing
                .get("instance")
                .context("instance")?
                .as_str()
                .context("instance string")?;
            if turn == 0 {
                let mut c = root.command(&zor);
                c.args(["worktree", "create", "tree", "--repo"])
                    .arg(path.join("work"))
                    .args(["--branch", "worker"]);
                let r = runtime::output(c, Duration::from_secs(12))?;
                ensure!(r.status.success(), "worktree create");
                let v: Value = serde_json::from_slice(&r.stdout)?;
                evidence["worktree"] = v.get("worktree").context("worktree")?.clone();
                let mut args = vec![
                    "start",
                    "worker",
                    "--title",
                    "resume fixture",
                    "--integration",
                    "opencode",
                    "--instance",
                    instance,
                    "--workspace",
                    "default",
                    "--worktree",
                    "tree",
                    "--",
                ];
                args.extend(argv);
                task(&args, true)?;
                task(
                    &["require-check", "worker", "verify", "--", "/usr/bin/true"],
                    true,
                )?;
            } else {
                ensure!(
                    task(&["launch-reconcile", "worker"], false)?
                        .as_str()
                        .context("loss")?
                        .contains("ownership lost"),
                    "ownership loss"
                );
                evidence["loss"] = task(&["inspect", "worker"], true)?;
                let saved = fs::read(&journal)?;
                let mut rejected = Vec::new();
                for (kind, expected) in [
                    ("metadata", "storage metadata unavailable"),
                    ("cancelled", "open task"),
                    ("terminator", "session selection flags"),
                ] {
                    let mut state: Value = serde_json::from_slice(&saved)?;
                    match kind {
                        "metadata" => {
                            state["launches"]["worker"]["integration"]["storage_environment"] =
                                Value::Null;
                            fs::write(&journal, serde_json::to_vec(&state)?)?;
                        }
                        "cancelled" => {
                            task(&["cancel", "worker"], true)?;
                        }
                        _ => {
                            state["launches"]["worker"]["argv"]
                                .as_array_mut()
                                .context("argv")?
                                .push(json!("--"));
                            fs::write(&journal, serde_json::to_vec(&state)?)?;
                        }
                    }
                    refusal(
                        &[
                            "resume",
                            "worker",
                            "--operation",
                            &format!("refused-{kind}"),
                            "--instance",
                            instance,
                        ],
                        expected,
                    )?;
                    rejected.push(kind);
                    fs::write(&journal, &saved)?;
                }
                evidence["refused_cases"] = json!(rejected);
                proxy = Some(Proxy::start(&control)?);
                evidence["dropped_reply"] = task(
                    &[
                        "resume",
                        "worker",
                        "--operation",
                        "resume-one",
                        "--instance",
                        instance,
                    ],
                    false,
                )?;
                let pending = task(&["inspect", "resume-one"], true)?;
                ensure!(
                    pending["launch"]["phase"] == "uncertain",
                    "creation certainty"
                );
                ensure!(
                    pending
                        .pointer("/launch/resume/previous_attempt")
                        .context("previous attempt")?
                        == evidence
                            .pointer("/loss/attempt/id")
                            .context("lost attempt")?,
                    "resume previous attempt"
                );
                task(
                    &[
                        "resume",
                        "worker",
                        "--operation",
                        "resume-one",
                        "--instance",
                        instance,
                    ],
                    true,
                )?;
            }
            let registered = until(owner, || {
                let v = task(&["inspect", "worker"], true)?;
                Ok(v.pointer("/launch/integration/producer")
                    .is_some_and(|v| !v.is_null() && v != &json!(""))
                    .then_some(v))
            })?;
            evidence["zor_attempts"]
                .as_array_mut()
                .unwrap()
                .push(registered.clone());
            if turn > 0 {
                evidence["resume_retry"] = task(
                    &[
                        "resume",
                        "worker",
                        "--operation",
                        "resume-one",
                        "--instance",
                        instance,
                    ],
                    true,
                )?;
                refusal(
                    &[
                        "resume",
                        "worker",
                        "--operation",
                        "resume-one",
                        "--instance",
                        "foreign",
                    ],
                    "different intent",
                )?;
                let state = read_journal()?;
                evidence["old_prompt_after"] = state["prompts"]["prompt-0"].clone();
                evidence["pending_after"] = state["prompts"]["never-replay"].clone();
            }
            let listing = runtime::rpc(&control, json!({"id":1,"command":"list"}))?;
            let tabs = listing
                .pointer("/workspaces/0/tabs")
                .and_then(Value::as_array)
                .context("tabs")?;
            let pane = tabs
                .iter()
                .filter_map(|t| t["panes"].as_array())
                .flatten()
                .find(|p| p.get("id") == registered.pointer("/session/target/pane"))
                .context("owned pane")?;
            evidence["targets"].as_array_mut().unwrap().push(json!({"instance":instance,"pane":pane,"argv":registered.pointer("/launch/argv").context("argv")?}));
            let value = |command: &str, mut fields: Value| -> Result<Value> {
                fields["id"] = json!(1);
                fields["command"] = json!(command);
                fields["instance"] = json!(instance);
                runtime::rpc(&control, fields)
            };
            let capture = || {
                value(
                    "capture",
                    json!({"pane":pane.get("id").context("pane id")?,"max_bytes":131072}),
                )
            };
            until(owner, || {
                Ok(capture()?["text"]
                    .as_str()
                    .context("text")?
                    .contains(if turn == 0 {
                        "Ask anything"
                    } else {
                        "RESUME_REPLY_1"
                    })
                    .then_some(()))
            })?;
            let before = capture()?;
            ensure!(
                before["input_sequence"] == 0,
                "resume replayed terminal input"
            );
            evidence["captures"].as_array_mut().unwrap().push(before);
            let prompt_id = format!("prompt-{turn}");
            task(
                &[
                    "prepare",
                    "worker",
                    "--operation",
                    &prompt_id,
                    "--text",
                    prompt,
                ],
                true,
            )?;
            let submitted = task(&["submit", &prompt_id], true)?;
            let operation = submitted
                .pointer("/receipt/operation")
                .context("operation")?;
            let message = until(owner, || {
                let e = events(&path.join("events.jsonl"))?;
                let rows = e.as_array().context("events")?;
                let chats = rows
                    .iter()
                    .filter(|r| r["hook"] == "chat.message")
                    .collect::<Vec<_>>();
                if chats.len() != turn + 1 {
                    return Ok(None);
                }
                let message = chats
                    .last()
                    .context("chat")?
                    .pointer("/output/message")
                    .context("message")?;
                let id = message.get("id").context("message id")?;
                Ok(rows
                    .iter()
                    .any(|r| {
                        r.pointer("/event/type") == Some(&json!("message.updated"))
                            && r.pointer("/event/properties/info/parentID") == Some(id)
                            && r.pointer("/event/properties/info/time/completed")
                                .is_some_and(|v| !v.is_null() && v != &json!(0))
                            && r.pointer("/event/properties/info/finish") == Some(&json!("stop"))
                    })
                    .then_some(message.clone()))
            })?;
            let next = message.get("sessionID").context("sessionID")?;
            if let Some(session) = &session {
                ensure!(next == session, "session changed");
            }
            session = Some(next.clone());
            until(owner, || {
                Ok(capture()?["text"]
                    .as_str()
                    .context("text")?
                    .contains(&format!("RESUME_REPLY_{}", turn + 1))
                    .then_some(()))
            })?;
            let status = value("input-status", json!({"operation":operation}))?;
            evidence["submissions"].as_array_mut().unwrap().push(
                json!({"message":message,"receipt":status.get("receipt").context("receipt")?}),
            );
            evidence["captures"]
                .as_array_mut()
                .unwrap()
                .push(capture()?);
            let result = until(owner, || {
                let v = task(&["wait", &prompt_id], true)?;
                Ok((v["wait"] == "response-observed").then_some(v))
            })?;
            evidence["zor_outcomes"]
                .as_array_mut()
                .unwrap()
                .push(result);
            if turn == 0 {
                refusal(
                    &[
                        "resume",
                        "worker",
                        "--operation",
                        "too-early",
                        "--instance",
                        instance,
                    ],
                    "lost or finished",
                )?;
                task(
                    &[
                        "prepare",
                        "worker",
                        "--operation",
                        "never-replay",
                        "--text",
                        "NEVER REPLAY",
                    ],
                    true,
                )?;
                task(&["reserve", "never-replay"], true)?;
                let state = read_journal()?;
                evidence["old_prompt_before"] = state["prompts"]["prompt-0"].clone();
                evidence["pending_before"] = state["prompts"]["never-replay"].clone();
            } else {
                task(&["source-collect", "worker", "resumed-source"], true)?;
                task(
                    &[
                        "check",
                        "worker",
                        "resumed-check",
                        "--source",
                        "resumed-source",
                        "--requirement",
                        "verify",
                        "--",
                        "/usr/bin/true",
                    ],
                    true,
                )?;
                evidence["verification"] = task(&["verify", "worker", "resumed-source"], true)?;
                evidence["verified_task"] = task(&["inspect", "worker"], true)?
                    .get("task")
                    .context("task")?
                    .clone();
                let mut c = root.command(&zor);
                c.arg("serve")
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null());
                let mut dashboard = Owner(c.spawn()?);
                let observation = (|| -> Result<()> {
                    until(owner, || {
                        Ok(path.join("zor/control.sock").exists().then_some(()))
                    })?;
                    let mut c = root.command(&zor);
                    c.args(["dashboard", "--once"]);
                    let r = runtime::output(c, Duration::from_secs(12))?;
                    ensure!(r.status.success(), "dashboard");
                    evidence["dashboard"] = serde_json::from_slice(&r.stdout)?;
                    let state = read_journal()?;
                    evidence["historical_launches"] = json!(
                        state["launches"]
                            .as_object()
                            .context("launches")?
                            .iter()
                            .filter(|(_, l)| l
                                .get("task")
                                .is_some_and(|v| !v.is_null() && v != &json!("")))
                            .map(|(k, _)| k)
                            .collect::<Vec<_>>()
                    );
                    Ok(())
                })();
                let stopped = dashboard.stop();
                observation?;
                ensure!(stopped?.success(), "dashboard cleanup");
                ensure!(
                    read_journal()?["prompts"]["never-replay"] == evidence["pending_before"],
                    "old reservation mutated"
                );
            }
            if let Some(mut p) = proxy.take() {
                evidence["creation_requests"] = json!(p.count());
                p.close()?;
            }
            stop(&mut process, &mut evidence)?;
        }
        evidence["session"] = json!(session);
        Ok(())
    })();
    let mut errors = Vec::new();
    if let Some(mut p) = proxy.take() {
        evidence["creation_requests"] = json!(p.count());
        if let Err(e) = p.close() {
            errors.push(format!("proxy cleanup: {e:#}"));
        }
    }
    if let Err(e) = stop(&mut process, &mut evidence) {
        errors.push(format!("fux cleanup: {e:#}"));
    }
    let stopped = provider.close();
    evidence["provider_stopped"] = json!(stopped.is_ok());
    if let Err(e) = stopped {
        errors.push(format!("provider cleanup: {e:#}"));
    }
    evidence["requests"] = json!(provider.records());
    match events(&path.join("events.jsonl")) {
        Ok(v) => evidence["events"] = v,
        Err(e) => errors.push(format!("events: {e:#}")),
    }
    evidence = normalized(&evidence, &path)?;
    if journal.exists() {
        let mut encoded = serde_json::to_string(&evidence)?;
        for prompt in read_journal()?["prompts"]
            .as_object()
            .context("prompts")?
            .values()
        {
            if let Some(token) = prompt
                .get("report_token")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
            {
                encoded = encoded.replace(token, "<PROMPT_TOKEN>");
            }
        }
        evidence = serde_json::from_str(&encoded)?;
    }
    let report = output.join("resume.json");
    fs::write(&report, serde_json::to_string_pretty(&evidence)? + "\n")?;
    outcome.context(format!("cleanup errors: {errors:?}"))?;
    ensure!(errors.is_empty(), "{}", errors.join("; "));
    resume::zor(&evidence)?;
    evidence["passed"] = json!(true);
    fs::write(&report, serde_json::to_string_pretty(&evidence)? + "\n")?;
    println!("{}", report.display());
    Ok(())
}
