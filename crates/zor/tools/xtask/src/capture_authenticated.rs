//! Explicitly requested provider captures; validation never invokes these paid workflows.
use crate::{
    authenticated,
    runtime::{self, Owner, Root},
};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Write,
    os::unix::fs::{DirBuilderExt, OpenOptionsExt},
    path::{Path, PathBuf},
    process::Stdio,
    time::{Duration, Instant},
};

#[derive(Clone, Copy, PartialEq)]
pub enum Kind {
    Codex,
    Approval,
    Claude,
}
impl Kind {
    fn prompt(self) -> &'static str {
        match self {
            Self::Codex => "Reply with exactly FUX_OBSERVATION_OK. Do not use any tools.",
            Self::Claude => "Reply with exactly FUX_CLAUDE_OBSERVATION_OK. Do not use tools.",
            Self::Approval => {
                "This is an approval UI test. Request approval to run /usr/bin/true outside the sandbox, with justification: May I run the harmless approval fixture? Do not execute any other command or tool. Wait for the user to approve; do not bypass approval."
            }
        }
    }
    fn validate(self, value: &Value) -> Result<()> {
        match self {
            Self::Codex => authenticated::codex(value),
            Self::Approval => authenticated::approval(value),
            Self::Claude => authenticated::claude(value),
        }
    }
}
fn strings(value: &Value, result: &mut Vec<String>) {
    match value {
        Value::String(s) if s.chars().count() >= 12 => result.push(s.clone()),
        Value::Array(a) => {
            for v in a {
                strings(v, result);
            }
        }
        Value::Object(o) => {
            for v in o.values() {
                strings(v, result);
            }
        }
        _ => {}
    }
}
fn sanitize(value: &Value, root: &Path, kind: Kind, secrets: &[String]) -> Result<String> {
    if kind == Kind::Claude {
        let encoded = serde_json::to_string_pretty(&authenticated::redact(value, root)?)?;
        ensure!(
            !secrets.iter().any(|secret| encoded.contains(secret)),
            "credential material detected; refusing publication"
        );
        return Ok(encoded);
    }
    let encoded = serde_json::to_string_pretty(value)?;
    ensure!(
        !secrets.iter().any(|secret| encoded.contains(secret)),
        "credential material detected; refusing publication"
    );
    let encoded = encoded
        .replace(
            root.canonicalize()?.to_str().context("canonical root")?,
            "<CAPTURE_ROOT>",
        )
        .replace(root.to_str().context("root")?, "<CAPTURE_ROOT>");
    Ok(regex::Regex::new(r"[\p{L}\p{N}_.+-]+@[\p{L}\p{N}_.-]+")?
        .replace_all(&encoded, "<EMAIL>")
        .into_owned())
}
fn submit(
    control: &Path,
    instance: &Value,
    pane: &Value,
    evidence: &mut Value,
    keys: &str,
    purpose: &str,
) -> Result<()> {
    let receipt = runtime::rpc(
        control,
        json!({"id":1,"command":"input-reserve","instance":instance,"pane":pane,"retain_ms":60000}),
    )?["receipt"]
        .clone();
    let result = runtime::rpc(
        control,
        json!({"id":1,"command":"input-submit","instance":instance,"operation":receipt["operation"],"keys":keys}),
    )?;
    evidence["inputs"]
        .as_array_mut()
        .context("input records")?
        .push(json!({"purpose":purpose,"keys":keys,"result":result}));
    Ok(())
}
pub fn run(kind: Kind, args: Vec<String>) -> Result<()> {
    let mut values = BTreeMap::new();
    let mut args = args.into_iter();
    while let Some(key) = args.next() {
        ensure!(
            ["--fux", "--agent", "--auth-file", "--output", "--model"].contains(&key.as_str()),
            "unknown option {key}"
        );
        ensure!(
            values
                .insert(key, args.next().context("missing option value")?)
                .is_none(),
            "duplicate option"
        );
    }
    let fux = Path::new(values.get("--fux").context("--fux required")?).canonicalize()?;
    let agent = std::path::absolute(values.get("--agent").context("--agent required")?)?;
    agent.canonicalize()?;
    let model = values.get("--model").context("--model required")?;
    let output = PathBuf::from(values.get("--output").context("--output required")?);
    let mut secrets = Vec::new();
    let mut auth = None;
    let mut key = None;
    if kind == Kind::Claude {
        let value = std::env::var("ANTHROPIC_API_KEY").context("missing exported Anthropic key")?;
        ensure!(
            value.chars().count() >= 16,
            "missing usable exported Anthropic key"
        );
        secrets.push(value.clone());
        for count in [16, 8] {
            secrets.push(
                value
                    .chars()
                    .rev()
                    .take(count)
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev()
                    .collect(),
            );
        }
        key = Some(value);
    } else {
        let bytes = fs::read(values.get("--auth-file").context("--auth-file required")?)?;
        strings(&serde_json::from_slice::<Value>(&bytes)?, &mut secrets);
        auth = Some(bytes);
    }
    if let Some(parent) = output.parent().filter(|p| !p.as_os_str().is_empty()) {
        fs::create_dir_all(parent)?;
    }
    fs::DirBuilder::new().mode(0o700).create(&output)?;
    let mut root = Root::new(if kind == Kind::Claude {
        "zcla-rs-"
    } else {
        "zca-rs-"
    })?;
    let auth_path = root.path().join("codex/auth.json");
    if let Some(bytes) = auth {
        fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&auth_path)?
            .write_all(&bytes)?;
        fs::write(
            root.path().join("codex/config.toml"),
            "cli_auth_credentials_store = \"file\"\ncheck_for_update_on_startup = false\n",
        )?;
    }
    if let Some(key) = key {
        root.set_env("ANTHROPIC_API_KEY", key);
    }
    let program = agent.to_str().context("agent path")?;
    let argv: Vec<&str> = if kind == Kind::Claude {
        vec![
            program,
            "--bare",
            "--model",
            model,
            "--tools",
            "",
            "--",
            kind.prompt(),
        ]
    } else {
        vec![
            program,
            "-m",
            model,
            "-s",
            "read-only",
            "-a",
            if kind == Kind::Approval {
                "on-request"
            } else {
                "never"
            },
            "--no-alt-screen",
            kind.prompt(),
        ]
    };
    let trigger = match kind {
        Kind::Codex => "Codex /quit",
        Kind::Approval => "explicit fux kill; approval unanswered",
        Kind::Claude => "Claude /exit",
    };
    let isolation = if kind == Kind::Claude {
        "Fresh HOME/XDG/CLAUDE_CONFIG_DIR/work; only authorized ANTHROPIC_API_KEY inherited; bare mode with all tools disabled."
    } else if kind == Kind::Approval {
        "Fresh HOME/XDG/CODEX_HOME/work; only authorized auth.json copied; read-only sandbox, approval on-request."
    } else {
        "Fresh HOME/XDG/CODEX_HOME/work; only authorized auth.json copied; read-only sandbox, approval never."
    };
    let mut evidence = json!({"schema":1,"captured_at":time::OffsetDateTime::now_utc().format(&time::format_description::well_known::Rfc3339)?,"agent_version":"","binary_sha256":runtime::hash(&agent)?,"fux_sha256":runtime::hash(&fux)?,"harness_sha256":runtime::hash(&std::env::current_exe()?)?,"harness_kind":"rust-binary","model":model,"prompt":kind.prompt(),"provider":if kind==Kind::Claude{"real authenticated Anthropic; no synthetic provider"}else{"real authenticated OpenAI; no synthetic provider"},"isolation":isolation,"limitation":"Passive viewport observations; no native prompt binding or verified task outcome.","captures":[],"inputs":[],"exit_trigger":trigger,"final":null,"argv":argv});
    let mut command = root.command(&agent);
    command.arg("--version");
    let version = runtime::output(command, Duration::from_secs(10))?;
    ensure!(version.status.success(), "agent version failed");
    evidence["agent_version"] = String::from_utf8(version.stdout)?.trim().into();
    fs::write(
        root.path().join("config/fux/config.toml"),
        format!(
            "default-command = {{ argv = {} }}\n",
            serde_json::to_string(&argv)?
        ),
    )?;
    let mut process = Owner(
        root.command(&fux)
            .arg("serve")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?,
    );
    let control = root.path().join("fux/default.sock");
    let mut pane = Value::Null;
    let mut instance = Value::Null;
    let started = Instant::now();
    let scenario = (|| -> Result<()> {
        while !control.exists() {
            ensure!(
                process.0.try_wait()?.is_none() && started.elapsed() < Duration::from_secs(10),
                "fux startup failed"
            );
            std::thread::sleep(Duration::from_millis(100));
        }
        let listing = runtime::rpc(&control, json!({"id":1,"command":"list"}))?;
        instance = listing["instance"].clone();
        pane = listing["workspaces"][0]["tabs"][0]["panes"][0]["id"].clone();
        evidence["instance"] = instance.clone();
        evidence["pane"] = pane.clone();
        let command = |name: &str, mut fields: Value| {
            fields["id"] = 1.into();
            fields["command"] = name.into();
            fields["instance"] = instance.clone();
            runtime::rpc(&control, fields)
        };
        let mut response_at = None;
        let mut actions = BTreeSet::new();
        let response = regex::Regex::new(r"(?m)^\s*[•●]\s+FUX_OBSERVATION_OK\s*$")?;
        loop {
            ensure!(
                started.elapsed() < Duration::from_secs(60),
                "no observed response or approval within 60 seconds"
            );
            let capture = command("capture", json!({"pane":pane,"max_bytes":131072}))?;
            let text = capture["text"].as_str().context("capture text")?.to_owned();
            let captures = evidence["captures"].as_array_mut().context("captures")?;
            if captures
                .last()
                .is_none_or(|last| last["capture"]["text"] != text)
            {
                ensure!(captures.len() < 600, "capture count limit");
                captures
                    .push(json!({"elapsed_ms":started.elapsed().as_millis(),"capture":capture}));
            }
            if kind == Kind::Claude {
                for (purpose, marker, keys) in [
                    ("theme", "Choose the text style that looks best", vec!["\r"]),
                    (
                        "key",
                        "Detected a custom API key in your environment",
                        vec!["\x1b[A", "\r"],
                    ),
                    ("security", "Security notes:", vec!["\r"]),
                    (
                        "directory",
                        "Yes, I trust this folder",
                        vec!["\x1b[B", "\r"],
                    ),
                ] {
                    if text.contains(marker) && !actions.contains(purpose) {
                        std::thread::sleep(Duration::from_millis(500));
                        for key in keys {
                            submit(&control, &instance, &pane, &mut evidence, key, purpose)?;
                            std::thread::sleep(Duration::from_millis(300));
                        }
                        actions.insert(purpose);
                    }
                }
            } else if text.contains("Do you trust the contents of this directory?")
                && text.contains("1. Yes, continue")
                && evidence["inputs"].as_array().context("inputs")?.is_empty()
            {
                if kind == Kind::Approval {
                    std::thread::sleep(Duration::from_millis(500));
                }
                submit(
                    &control,
                    &instance,
                    &pane,
                    &mut evidence,
                    "\r",
                    "trust only the disposable fixture directory",
                )?;
            }
            let ready = match kind {
                Kind::Claude => {
                    text.contains("⏺ FUX_CLAUDE_OBSERVATION_OK")
                        && !text.contains("esc to interrupt")
                }
                Kind::Codex => response.is_match(&text),
                Kind::Approval => [
                    "Would you like to run the following command?",
                    "Press enter to confirm or esc to cancel",
                    "Allow command?",
                ]
                .iter()
                .any(|m| text.contains(m)),
            };
            if ready {
                let since = response_at.get_or_insert_with(Instant::now);
                if since.elapsed() > Duration::from_secs(2) {
                    break;
                }
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        if kind == Kind::Approval {
            command("kill", json!({"pane":pane}))?;
        } else {
            let (exit, name) = if kind == Kind::Claude {
                ("/exit", "Claude")
            } else {
                ("/quit", "Codex")
            };
            submit(
                &control,
                &instance,
                &pane,
                &mut evidence,
                exit,
                &format!("type {name} exit command"),
            )?;
            std::thread::sleep(Duration::from_millis(500));
            submit(
                &control,
                &instance,
                &pane,
                &mut evidence,
                "\r",
                &format!("submit {name} exit command"),
            )?;
        }
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            let mut final_command = root.command(&fux);
            final_command.args([
                "final",
                "--instance",
                instance.as_str().context("instance")?,
                &pane.to_string(),
            ]);
            let result = runtime::output(final_command, Duration::from_secs(3))?;
            if result.status.success() {
                evidence["final"] =
                    serde_json::from_slice::<Value>(&result.stdout)?["result"]["value"]["record"]
                        .clone();
                evidence
                    .as_object_mut()
                    .context("evidence")?
                    .remove("final_problem");
                break;
            }
            if let Ok(capture) = command("capture", json!({"pane":pane,"max_bytes":131072})) {
                evidence["after_quit"] = capture;
            }
            let mut bytes = result.stdout;
            bytes.extend(result.stderr);
            evidence["final_problem"] = String::from_utf8_lossy(&bytes)
                .chars()
                .take(1024)
                .collect::<String>()
                .into();
            std::thread::sleep(Duration::from_millis(100));
        }
        ensure!(!evidence["final"].is_null(), "final record missing");
        Ok(())
    })();
    let mut failed = scenario.is_err();
    if failed {
        evidence["failure"] = "CaptureError".into();
    }
    if evidence["final"].is_null() && !pane.is_null() && process.0.try_wait()?.is_none() {
        let _ = runtime::rpc(
            &control,
            json!({"id":1,"command":"kill","instance":instance,"pane":pane}),
        );
    }
    if process.stop().is_err() {
        failed = true;
        evidence["forced_server_cleanup"] = true.into();
    }
    let status = process
        .0
        .try_wait()?
        .context("capture server remains unreaped")?;
    evidence["server_exit"] = json!(status.code().unwrap_or(-1));
    if !status.success() {
        failed = true;
    }
    if kind != Kind::Claude && auth_path.exists() {
        strings(
            &serde_json::from_slice::<Value>(&fs::read(&auth_path)?)?,
            &mut secrets,
        );
    }
    let encoded = sanitize(&evidence, root.path(), kind, &secrets)?;
    if !failed && kind.validate(&evidence).is_err() {
        failed = true;
    }
    let destination = output.join(if failed {
        "diagnostic.json"
    } else if kind == Kind::Approval {
        "approval.json"
    } else {
        "authenticated.json"
    });
    fs::write(&destination, encoded + "\n")?;
    ensure!(!failed, "capture failed; sanitized diagnostic retained");
    println!("{}", destination.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn secret_material_is_refused_and_previews_are_redacted() -> Result<()> {
        let root = tempfile::tempdir_in("/tmp")?;
        let secret = "EXAMPLE_SECRET_ONLY_FOR_TEST".to_owned();
        ensure!(
            sanitize(
                &json!({"text":secret}),
                root.path(),
                Kind::Codex,
                std::slice::from_ref(&secret)
            )
            .is_err(),
            "secret leaked"
        );
        ensure!(
            sanitize(
                &json!({"text":secret}),
                root.path(),
                Kind::Claude,
                std::slice::from_ref(&secret)
            )
            .is_err(),
            "secret leaked"
        );
        let text = sanitize(
            &json!({"text":"ANTHROPIC_API_KEY: sk-ant-...preview\nperson@example.com"}),
            root.path(),
            Kind::Claude,
            &["preview".into()],
        )?;
        ensure!(
            text.contains("<API_KEY_REDACTED>") && text.contains("<EMAIL>"),
            "preview redaction"
        );
        Ok(())
    }
}
