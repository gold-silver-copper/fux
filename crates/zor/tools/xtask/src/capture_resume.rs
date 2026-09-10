//! Real native session resume across owned fux lifetimes; independent of zor policy.
use crate::{
    capture_native::{events, normalized, until},
    native_provider::Provider,
    resume,
    runtime::{self, Owner, Root},
};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap, fs, os::unix::fs::DirBuilderExt, path::Path, process::Stdio,
    time::Duration,
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
        args.len() == 6,
        "usage: capture-opencode-resume --fux PATH --opencode PATH --output NEW_DIRECTORY"
    );
    let mut flags = BTreeMap::new();
    for p in args.as_chunks::<2>().0 {
        ensure!(
            ["--fux", "--opencode", "--output"].contains(&p[0].as_str()),
            "unknown flag"
        );
        ensure!(
            flags.insert(p[0].as_str(), p[1].as_str()).is_none(),
            "duplicate flag"
        );
    }
    let fux = Path::new(flags.get("--fux").context("fux")?).canonicalize()?;
    let agent = Path::new(flags.get("--opencode").context("opencode")?).canonicalize()?;
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
    let mut evidence = json!({"fux_sha256":runtime::hash(&fux)?,"opencode_sha256":runtime::hash(&agent)?,"harness_kind":"rust-binary","harness_sha256":runtime::hash(&std::env::current_exe()?)?,"requests":[],"captures":[],"targets":[],"submissions":[],"exits":[],"limitation":"Real OpenCode TUI and persisted session, synthetic provider; zor resume policy is not exercised."});
    let control = path.join("fux/default.sock");
    let mut process = None;
    let outcome = (|| -> Result<()> {
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
            let mut argv = json!([agent, path.join("work"), "--model", "fixture/fixture"]);
            if let Some(session) = &session {
                argv.as_array_mut()
                    .unwrap()
                    .extend([json!("--session"), session.clone()]);
            }
            fs::write(
                path.join("config/fux/config.toml"),
                format!("default-command = {{ argv = {argv} }}\n"),
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
            let pane = listing
                .pointer("/workspaces/0/tabs/0/panes/0")
                .context("pane")?;
            let instance = listing.get("instance").context("instance")?;
            evidence["targets"]
                .as_array_mut()
                .unwrap()
                .push(json!({"instance":instance,"pane":pane,"argv":argv}));
            let value = |command: &str, mut fields: Value| -> Result<Value> {
                fields["id"] = json!(1);
                fields["command"] = json!(command);
                fields["instance"] = instance.clone();
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
            let reserved = value(
                "input-reserve",
                json!({"pane":pane.get("id").context("pane id")?}),
            )?;
            let operation = reserved
                .pointer("/receipt/operation")
                .context("operation")?;
            value(
                "input-submit",
                json!({"operation":operation,"keys":format!("{prompt}\r")}),
            )?;
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
            stop(&mut process, &mut evidence)?;
        }
        evidence["session"] = json!(session);
        Ok(())
    })();
    let mut errors = Vec::new();
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
    let report = output.join("resume.json");
    fs::write(&report, serde_json::to_string_pretty(&evidence)? + "\n")?;
    outcome.context(format!("cleanup errors: {errors:?}"))?;
    ensure!(errors.is_empty(), "{}", errors.join("; "));
    resume::native(&evidence)?;
    evidence["passed"] = json!(true);
    fs::write(&report, serde_json::to_string_pretty(&evidence)? + "\n")?;
    println!("{}", report.display());
    Ok(())
}
