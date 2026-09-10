//! Offline validation of retained provider traces; never reads credentials or calls a model.
use crate::runtime::hash;
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{fs, path::Path};

fn field<'a>(value: &'a Value, path: &str) -> Result<&'a Value> {
    value
        .pointer(path)
        .with_context(|| format!("missing evidence {path}"))
}
fn truthy(value: Option<&Value>) -> bool {
    match value {
        None | Some(Value::Null) => false,
        Some(Value::Bool(v)) => *v,
        Some(Value::Number(v)) => v.as_f64() != Some(0.),
        Some(Value::String(v)) => !v.is_empty(),
        Some(Value::Array(v)) => !v.is_empty(),
        Some(Value::Object(v)) => !v.is_empty(),
    }
}
fn common<'a>(
    value: &'a Value,
    provider: &str,
    trigger: &str,
    inputs: &[&str],
    exit: Value,
) -> Result<Vec<&'a str>> {
    ensure!(
        field(value, "/provider")? == provider
            && field(value, "/server_exit")? == 0
            && !truthy(value.get("forced_server_cleanup"))
            && !truthy(value.get("failure")),
        "provider or cleanup claim"
    );
    ensure!(
        field(value, "/exit_trigger")? == trigger,
        "unsupported exit trigger"
    );
    let actual = field(value, "/inputs")?
        .as_array()
        .context("inputs")?
        .iter()
        .map(|i| field(i, "/keys"))
        .collect::<Result<Vec<_>>>()?;
    ensure!(
        actual
            == inputs
                .iter()
                .map(|s| json!(s))
                .collect::<Vec<_>>()
                .iter()
                .collect::<Vec<_>>(),
        "input history changed"
    );
    ensure!(
        field(value, "/final/pane")? == field(value, "/pane")?
            && field(value, "/final/command")? == field(value, "/argv")?,
        "final target/argv mismatch"
    );
    ensure!(
        field(value, "/final/exit_status")? == &exit
            && field(value, "/final/input_sequence")? == inputs.len(),
        "final exit/input mismatch"
    );
    let captures = field(value, "/captures")?.as_array().context("captures")?;
    ensure!(
        !captures.is_empty() && captures.len() <= 600,
        "capture bound"
    );
    captures
        .iter()
        .map(|item| {
            ensure!(
                field(item, "/capture/rows")? == 23 && field(item, "/capture/columns")? == 80,
                "capture geometry"
            );
            field(item, "/capture/text")?
                .as_str()
                .context("capture text")
        })
        .collect()
}
pub fn codex(value: &Value) -> Result<()> {
    let texts = common(
        value,
        "real authenticated OpenAI; no synthetic provider",
        "Codex /quit",
        &["\r", "/quit", "\r"],
        json!(0),
    )?;
    ensure!(
        texts
            .iter()
            .any(|t| t.contains("Do you trust the contents of this directory?")),
        "trust dialog missing"
    );
    let working = regex::Regex::new(r"(?m)^[•◦] Working \(")?;
    let response = regex::Regex::new(r"(?m)^• FUX_OBSERVATION_OK$")?;
    ensure!(
        texts.iter().any(|text| working.is_match(text)),
        "working observation missing"
    );
    ensure!(
        texts.iter().any(|text| response.is_match(text)),
        "response is missing or prompt echo"
    );
    Ok(())
}
pub fn approval(value: &Value) -> Result<()> {
    let texts = common(
        value,
        "real authenticated OpenAI; no synthetic provider",
        "explicit fux kill; approval unanswered",
        &["\r"],
        Value::Null,
    )?;
    ensure!(
        texts
            .iter()
            .any(|t| t.contains("Do you trust the contents of this directory?")),
        "trust dialog missing"
    );
    let captures = field(value, "/captures")?.as_array().context("captures")?;
    let last = captures.last().context("last capture")?;
    ensure!(
        field(last, "/capture/input_sequence")? == 1,
        "approval input changed"
    );
    let text = texts.last().context("last text")?;
    for marker in [
        "Would you like to run the following command?",
        "Press enter to confirm or esc to cancel",
        "Reason: May I run the harmless approval fixture?",
        "$ /usr/bin/true",
        "3. No, and tell Codex what to do differently (esc)",
    ] {
        ensure!(text.contains(marker), "approval dialog missing {marker}");
    }
    Ok(())
}
pub fn claude(value: &Value) -> Result<()> {
    let texts = common(
        value,
        "real authenticated Anthropic; no synthetic provider",
        "Claude /exit",
        &["\r", "\x1b[A", "\r", "\r", "\x1b[B", "\r", "/exit", "\r"],
        json!(0),
    )?;
    ensure!(
        texts.iter().any(|t| t.contains("esc to interrupt")),
        "working observation missing"
    );
    let last = texts.last().context("last capture")?;
    ensure!(
        last.contains("⏺ FUX_CLAUDE_OBSERVATION_OK")
            && !last.contains("esc to interrupt")
            && last.contains("? for shortcuts"),
        "response readiness absent or prompt echo"
    );
    Ok(())
}
pub fn redact(value: &Value, root: &Path) -> Result<Value> {
    let key = regex::Regex::new(r"(?m)ANTHROPIC_API_KEY: [^\n]*")?;
    let email = regex::Regex::new(r"[\p{L}\p{N}_.+-]+@[\p{L}\p{N}_.-]+")?;
    let resolved = root.canonicalize().or_else(|_| std::path::absolute(root))?;
    fn visit(
        value: &Value,
        key: &regex::Regex,
        email: &regex::Regex,
        root: &str,
        resolved: &str,
    ) -> Value {
        match value {
            Value::String(text) => {
                let text = key
                    .replace_all(text, "ANTHROPIC_API_KEY: <API_KEY_REDACTED>")
                    .replace(resolved, "<CAPTURE_ROOT>")
                    .replace(root, "<CAPTURE_ROOT>");
                email.replace_all(&text, "<EMAIL>").into_owned().into()
            }
            Value::Array(items) => items
                .iter()
                .map(|v| visit(v, key, email, root, resolved))
                .collect(),
            Value::Object(items) => Value::Object(
                items
                    .iter()
                    .map(|(k, v)| (k.clone(), visit(v, key, email, root, resolved)))
                    .collect(),
            ),
            other => other.clone(),
        }
    }
    Ok(visit(
        value,
        &key,
        &email,
        root.to_str().context("root UTF-8")?,
        resolved.to_str().context("resolved root UTF-8")?,
    ))
}
fn provenance(value: &Value, baseline: &Value, root: &Path, source: &str) -> Result<()> {
    ensure!(
        field(value, "/harness_sha256")?
            .as_str()
            .context("harness hash")?
            == hash(&root.join("tools/archive").join(format!("{source}.txt")))?,
        "historical harness changed"
    );
    ensure!(
        field(value, "/binary_sha256")? == field(baseline, "/binary_sha256")?,
        "binary provenance changed"
    );
    Ok(())
}
fn read(path: &Path) -> Result<Value> {
    Ok(serde_json::from_slice(&fs::read(path)?)?)
}
fn root() -> Result<&'static Path> {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .context("zor root")
}
pub fn codex_regressions() -> Result<()> {
    let root = root()?;
    let directory = root.join("tests/fixtures/agents/codex-0.153.4");
    let value = read(&directory.join("authenticated.json"))?;
    codex(&value)?;
    provenance(
        &value,
        &read(&directory.join("startup.json"))?,
        root,
        "capture_codex_authenticated.py",
    )?;
    for (field, replacement) in [
        ("exit_status", Value::Null),
        ("exit_status", json!(1)),
        ("pane", json!(9999)),
        ("input_sequence", json!(4)),
    ] {
        let mut bad = value.clone();
        bad["final"][field] = replacement;
        ensure!(codex(&bad).is_err(), "accepted false final {field}");
    }
    for (field, replacement) in [
        ("server_exit", json!(1)),
        ("forced_server_cleanup", json!(true)),
        ("provider", json!("synthetic")),
        ("exit_trigger", json!("fux kill")),
    ] {
        let mut bad = value.clone();
        bad[field] = replacement;
        ensure!(codex(&bad).is_err(), "accepted false {field}");
    }
    let mut echo = value.clone();
    for item in echo["captures"].as_array_mut().context("captures")? {
        item["capture"]["text"] = item["capture"]["text"]
            .as_str()
            .context("text")?
            .replace("• FUX_OBSERVATION_OK", "› FUX_OBSERVATION_OK")
            .into();
    }
    ensure!(codex(&echo).is_err(), "accepted prompt echo");
    let approved = read(&directory.join("approval.json"))?;
    approval(&approved)?;
    provenance(&approved, &value, root, "capture_codex_approval.py")?;
    let mut bad = approved.clone();
    let input = bad["inputs"][0].clone();
    bad["inputs"].as_array_mut().context("inputs")?.push(input);
    ensure!(approval(&bad).is_err(), "accepted answered dialog");
    let mut bad = approved.clone();
    bad["captures"]
        .as_array_mut()
        .context("captures")?
        .last_mut()
        .context("last capture")?["capture"]["text"] = "• Running /usr/bin/true".into();
    ensure!(approval(&bad).is_err(), "running label accepted as dialog");
    let mut bad = approved.clone();
    bad["final"]["exit_status"] = 0.into();
    ensure!(
        approval(&bad).is_err(),
        "forced release accepted as natural exit"
    );
    println!(
        "PASS retained Codex authenticated/approval traces, provenance and misleading-evidence mutations"
    );
    Ok(())
}
pub fn claude_regressions() -> Result<()> {
    let root = root()?;
    let directory = root.join("tests/fixtures/agents/claude-2.1.263");
    let value = read(&directory.join("authenticated.json"))?;
    claude(&value)?;
    provenance(
        &value,
        &read(&directory.join("startup.json"))?,
        root,
        "capture_claude_authenticated.py",
    )?;
    ensure!(
        field(&value, "/argv")?
            .as_array()
            .context("argv")?
            .contains(&json!("")),
        "empty argv entry lost"
    );
    let original = json!({"nested":[{"text":"  ANTHROPIC_API_KEY: sk-ant-...preview\nNext line\nperson@example.com"}]});
    let redacted = redact(&original, Path::new("/tmp/redaction-fixture"))?;
    ensure!(
        redacted["nested"][0]["text"]
            == "  ANTHROPIC_API_KEY: <API_KEY_REDACTED>\nNext line\n<EMAIL>"
            && original["nested"][0]["text"]
                .as_str()
                .context("original text")?
                .contains("preview"),
        "recursive redaction changed input or leaked preview"
    );
    for (field, replacement) in [
        ("exit_status", Value::Null),
        ("input_sequence", json!(9)),
        ("pane", json!(9999)),
    ] {
        let mut bad = value.clone();
        bad["final"][field] = replacement;
        ensure!(claude(&bad).is_err(), "accepted false final {field}");
    }
    for (old, new) in [
        ("⏺ FUX_CLAUDE_OBSERVATION_OK", "❯ FUX_CLAUDE_OBSERVATION_OK"),
        ("? for shortcuts", "esc to interrupt"),
    ] {
        let mut bad = value.clone();
        let capture = &mut bad["captures"]
            .as_array_mut()
            .context("captures")?
            .last_mut()
            .context("last capture")?["capture"];
        capture["text"] = capture["text"]
            .as_str()
            .context("text")?
            .replace(old, new)
            .into();
        ensure!(claude(&bad).is_err(), "accepted echo or active footer");
    }
    println!(
        "PASS retained Claude trace, provenance, recursive redaction and misleading-evidence mutations"
    );
    Ok(())
}
#[cfg(test)]
mod tests {
    #[test]
    fn codex() -> anyhow::Result<()> {
        super::codex_regressions()
    }
    #[test]
    fn claude() -> anyhow::Result<()> {
        super::claude_regressions()
    }
}
