//! Real OpenCode hooks with the deterministic loopback provider; no task-success claim.
use crate::{
    native_events,
    native_provider::Provider,
    runtime::{self, Owner, Root},
};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs,
    io::Read,
    os::unix::fs::PermissionsExt,
    path::Path,
    process::Stdio,
    time::{Duration, Instant},
};
const PROMPTS: [&str; 2] = [
    "Reply with FIXTURE RESPONSE",
    "ZOR_TOOL_STOP: inspect the fixture",
];
pub(super) fn events(path: &Path) -> Result<Value> {
    let mut bytes = Vec::new();
    match fs::File::open(path) {
        Ok(file) => {
            file.take(1048577).read_to_end(&mut bytes)?;
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(json!([])),
        Err(e) => return Err(e.into()),
    }
    ensure!(bytes.len() <= 1048576, "fixture event limit");
    let mut rows = Vec::new();
    if let Some(last) = bytes.iter().rposition(|b| *b == b'\n') {
        for line in bytes[..last].split(|b| *b == b'\n') {
            rows.push(serde_json::from_slice::<Value>(line)?);
        }
    }
    Ok(json!(rows))
}
pub(super) fn until<T>(
    process: &mut Owner,
    mut check: impl FnMut() -> Result<Option<T>>,
) -> Result<T> {
    let end = Instant::now() + Duration::from_secs(30);
    loop {
        if let Some(v) = check()? {
            return Ok(v);
        }
        ensure!(
            process.0.try_wait()?.is_none(),
            "fux exited before observation"
        );
        ensure!(
            Instant::now() < end,
            "fixture observation exceeded 30 seconds"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}
pub(super) fn normalized(e: &Value, root: &Path) -> Result<Value> {
    Ok(serde_json::from_str(&serde_json::to_string(e)?.replace(
        root.to_str().context("root")?,
        "<FIXTURE_ROOT>",
    ))?)
}
pub fn run(args: Vec<String>) -> Result<()> {
    ensure!(
        args.len() == 6,
        "usage: capture-opencode-events --fux PATH --opencode PATH --output NEW_DIRECTORY"
    );
    let mut flags = BTreeMap::new();
    for p in args.chunks_exact(2) {
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
    let opencode = Path::new(flags.get("--opencode").context("opencode")?).canonicalize()?;
    let output = Path::new(flags.get("--output").context("output")?);
    ensure!(!output.exists(), "use new output directory");
    if let Some(parent) = output.parent().filter(|p| !p.as_os_str().is_empty()) {
        fs::create_dir_all(parent)?;
    }
    fs::DirBuilder::new().create(output)?;
    fs::set_permissions(output, fs::Permissions::from_mode(0o700))?;
    let mut root = Root::new("zoe-rs-")?;
    let path = root.path().canonicalize()?;
    let fixture = path.join("work/fixture.txt");
    fs::write(&fixture, "Owned fixture.\n")?;
    let plugin = path.join("probe.js");
    fs::write(&plugin, include_bytes!("native-probe.mjs"))?;
    let mut provider = Provider::start(&fixture)?;
    let config = json!({"plugin":[format!("file://{}",plugin.display())],"model":"fixture/fixture","small_model":"fixture/fixture","enabled_providers":["fixture"],"permission":{"*":"deny","read":"allow"},"provider":{"fixture":{"npm":"@ai-sdk/openai-compatible","name":"Local Fixture","options":{"baseURL":format!("http://127.0.0.1:{}/v1",provider.port),"apiKey":"fixture"},"models":{"fixture":{"name":"Fixture","limit":{"context":65536,"output":1024}}}}}});
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
    let mut evidence = json!({"schema":1,"captured_at":time::OffsetDateTime::now_utc().format(&time::format_description::well_known::Rfc3339)?,"platform":std::env::consts::OS,"prompts":PROMPTS,"limitation":"Real OpenCode TUI with synthetic local provider. No zor report binding, cloud credentials, model-quality or task-success claim.","requests":[],"submissions":[],"captures":[],"harness_kind":"rust-binary","harness_sha256":runtime::hash(&std::env::current_exe()?)?,"plugin_sha256":runtime::hash(&plugin)?,"opencode_sha256":runtime::hash(&opencode)?,"fux_sha256":runtime::hash(&fux)?});
    let control = path.join("fux/default.sock");
    let mut pane = None;
    let mut instance = Value::Null;
    let mut process: Option<Owner> = None;
    let outcome = (|| -> Result<()> {
        let mut c = root.command(&opencode);
        c.arg("--version");
        let v = runtime::output(c, Duration::from_secs(10))?;
        ensure!(v.status.success(), "OpenCode version");
        evidence["opencode_version"] = String::from_utf8(v.stdout)?.trim().into();
        fs::write(
            path.join("config/fux/config.toml"),
            format!(
                "default-command = {{ argv = {} }}\n",
                json!([opencode, path.join("work"), "--model", "fixture/fixture"])
            ),
        )?;
        let mut c = root.command(&fux);
        c.arg("serve")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(fs::File::create(path.join("stderr"))?);
        process = Some(Owner(c.spawn()?));
        let process = process.as_mut().unwrap();
        until(process, || Ok(control.exists().then_some(())))?;
        let listing = runtime::rpc(&control, json!({"id":1,"command":"list"}))?;
        instance = listing.get("instance").context("instance")?.clone();
        pane = Some(
            listing
                .pointer("/workspaces/0/tabs/0/panes/0/id")
                .context("pane")?
                .clone(),
        );
        let value = |command: &str, mut fields: Value| -> Result<Value> {
            fields["id"] = json!(1);
            fields["command"] = json!(command);
            fields["instance"] = instance.clone();
            runtime::rpc(&control, fields)
        };
        let capture = || {
            value(
                "capture",
                json!({"pane":pane,"attrs":false,"scrollback":0,"max_bytes":131072}),
            )
        };
        let first = until(process, || {
            let v = capture()?;
            let text = v["text"].as_str().context("text")?;
            Ok((text.contains("Fixture") && text.contains("Ask anything")).then_some(v))
        })?;
        evidence["captures"].as_array_mut().unwrap().push(first);
        for (turn, prompt) in PROMPTS.iter().enumerate() {
            let reserved = value("input-reserve", json!({"pane":pane}))?;
            let operation = reserved
                .pointer("/receipt/operation")
                .context("operation")?;
            let submitted = value(
                "input-submit",
                json!({"operation":operation,"keys":format!("{prompt}\r")}),
            )?;
            evidence["submissions"]
                .as_array_mut()
                .unwrap()
                .push(submitted);
            until(process, || {
                let v = events(&path.join("events.jsonl"))?;
                Ok((v
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter(|e| e.pointer("/event/type") == Some(&json!("session.idle")))
                    .count()
                    > turn)
                    .then_some(()))
            })?;
            evidence["submissions"][turn]["final_receipt"] =
                value("input-status", json!({"operation":operation}))?
                    .get("receipt")
                    .context("receipt")?
                    .clone();
            evidence["captures"]
                .as_array_mut()
                .unwrap()
                .push(capture()?);
        }
        Ok(())
    })();
    let mut errors = Vec::new();
    if let Some(process) = process.as_mut() {
        let cleanup = (|| -> Result<()> {
            if process.0.try_wait()?.is_none()
                && let Some(pane) = pane
            {
                runtime::rpc(
                    &control,
                    json!({"id":1,"command":"kill","instance":instance,"pane":pane}),
                )?;
            }
            Ok(())
        })();
        if let Err(e) = cleanup {
            errors.push(format!("pane cleanup: {e:#}"));
        }
        match process.stop() {
            Ok(status) => {
                evidence["fux_exit"] = json!(status.code());
                if !status.success() {
                    errors.push(format!("fux exit: {status}"));
                }
            }
            Err(e) => errors.push(format!("fux cleanup: {e:#}")),
        }
    }
    let stopped = provider.close();
    evidence["provider_thread_stopped"] = json!(stopped.is_ok());
    if let Err(e) = stopped {
        errors.push(format!("provider cleanup: {e:#}"));
    }
    evidence["requests"] = json!(provider.records());
    match events(&path.join("events.jsonl")) {
        Ok(v) => evidence["events"] = v,
        Err(e) => errors.push(format!("events: {e:#}")),
    }
    evidence = normalized(&evidence, &path)?;
    fs::write(
        output.join("diagnostic.json"),
        serde_json::to_string_pretty(&evidence)? + "\n",
    )?;
    outcome.context(format!("cleanup errors: {errors:?}"))?;
    ensure!(errors.is_empty(), "{}", errors.join("; "));
    evidence["outcomes"] = native_events::validate(&evidence)?;
    fs::write(
        output.join("events.json"),
        serde_json::to_string_pretty(&evidence)? + "\n",
    )?;
    println!("{}", output.join("events.json").display());
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn complete_records_only_and_event_limit() -> Result<()> {
        let root = tempfile::tempdir()?;
        let p = root.path().join("events");
        ensure!(events(&p)? == json!([]), "missing event file");
        fs::write(&p, b"{\"hook\":\"loaded\"}\n{\"unfinished\":")?;
        ensure!(events(&p)? == json!([{"hook":"loaded"}]), "partial tail");
        fs::write(&p, b"invalid\n")?;
        ensure!(events(&p).is_err(), "malformed complete record");
        fs::write(&p, vec![b'x'; 1048577])?;
        ensure!(events(&p).is_err(), "event bound");
        Ok(())
    }
}
