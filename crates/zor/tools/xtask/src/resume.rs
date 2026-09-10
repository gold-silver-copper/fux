//! Offline native/zor resume evidence. No endpoint, provider or account access.
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{fs, path::Path};
fn field<'a>(v: &'a Value, p: &str) -> Result<&'a Value> {
    v.pointer(p).with_context(|| format!("missing {p}"))
}
fn array<'a>(v: &'a Value, p: &str) -> Result<&'a Vec<Value>> {
    field(v, p)?.as_array().context("array")
}
fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64() != Some(0.),
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
    }
}
const PROMPTS: [&str; 2] = [
    "RESUME_FIRST: reply briefly",
    "RESUME_SECOND: reply briefly",
];
pub(super) fn native(e: &Value) -> Result<()> {
    ensure!(
        field(e, "/version")? == "1.18.29"
            && field(e, "/exits")? == &json!([0, 0])
            && truthy(e.get("provider_stopped")),
        "version/cleanup"
    );
    let targets = array(e, "/targets")?;
    let submissions = array(e, "/submissions")?;
    let captures = array(e, "/captures")?;
    ensure!(
        targets.len() == 2 && submissions.len() == 2 && captures.len() == 4,
        "resume evidence shape"
    );
    ensure!(
        field(&targets[0], "/instance")? != field(&targets[1], "/instance")?
            && field(&targets[0], "/pane/pid")? != field(&targets[1], "/pane/pid")?,
        "server/agent not replaced"
    );
    let session = field(e, "/session")?;
    let argv = array(&targets[1], "/argv")?;
    ensure!(
        argv.len() >= 2 && argv[argv.len() - 2..] == [json!("--session"), session.clone()],
        "resume argv session"
    );
    ensure!(
        field(&submissions[0], "/message/id")? != field(&submissions[1], "/message/id")?,
        "native message reused"
    );
    let events = array(e, "/events")?;
    let chats = events
        .iter()
        .filter(|r| r.get("hook") == Some(&json!("chat.message")))
        .collect::<Vec<_>>();
    ensure!(chats.len() == 2, "native prompt replay/missing turn");
    for (i, submission) in submissions.iter().enumerate() {
        let receipt = field(submission, "/receipt")?;
        let message = field(submission, "/message")?;
        ensure!(
            field(message, "/sessionID")? == session
                && message == field(chats[i], "/output/message")?,
            "native session/message mismatch"
        );
        let mut texts = Vec::new();
        for part in array(chats[i], "/output/parts")? {
            if field(part, "/type")? == "text" {
                texts.push(field(part, "/text")?);
            }
        }
        ensure!(texts == vec![&json!(PROMPTS[i])], "native prompt text");
        ensure!(
            field(receipt, "/pane")? == field(&targets[i], "/pane/id")?
                && field(receipt, "/operation")?
                    .as_u64()
                    .context("operation")?
                    > 0
                && field(receipt, "/state")? == "delivered"
                && field(receipt, "/input_sequence")? == 1,
            "delivery receipt identity"
        );
        ensure!(
            field(receipt, "/bytes_written")? == PROMPTS[i].len() + 1
                && field(receipt, "/error")?.is_null(),
            "partial/error delivery"
        );
        ensure!(
            field(&captures[2 * i], "/input_sequence")? == 0
                && field(&captures[2 * i + 1], "/input_sequence")? == 1
                && field(&captures[2 * i + 1], "/text")?
                    .as_str()
                    .context("capture")?
                    .contains(&format!("RESUME_REPLY_{}", i + 1)),
            "capture input/response"
        );
        let mut completed = false;
        for row in events {
            if row.pointer("/event/type") == Some(&json!("message.updated")) {
                let info = field(row, "/event/properties/info")?;
                completed |= info.get("sessionID") == Some(session)
                    && info.get("parentID") == Some(field(message, "/id")?)
                    && truthy(info.pointer("/time/completed"))
                    && info.get("finish") == Some(&json!("stop"))
                    && !truthy(info.get("error"));
            }
        }
        ensure!(completed, "unrelated/incomplete response");
    }
    ensure!(
        field(&captures[2], "/text")?
            .as_str()
            .context("capture")?
            .contains("RESUME_REPLY_1"),
        "prior history absent before resumed input"
    );
    let mut history = false;
    for messages in array(e, "/requests")? {
        let messages = messages.as_array().context("messages")?;
        let has = |role: &str, text: &str| -> Result<bool> {
            for message in messages {
                if field(message, "/role")? == role
                    && message
                        .get("content")
                        .is_some_and(|c| c.to_string().contains(text))
                {
                    return Ok(true);
                }
            }
            Ok(false)
        };
        if has("user", "RESUME_FIRST")?
            && has("user", "RESUME_SECOND")?
            && has("assistant", "RESUME_REPLY_1")?
        {
            history = true;
            break;
        }
    }
    ensure!(history, "prior conversation absent from resumed request");
    ensure!(
        !events
            .iter()
            .any(|r| r.pointer("/event/type") == Some(&json!("session.error"))),
        "native session error"
    );
    Ok(())
}
pub(super) fn zor(e: &Value) -> Result<()> {
    native(e)?;
    let attempts = array(e, "/zor_attempts")?;
    ensure!(attempts.len() == 2, "attempt count");
    let (first, second) = (&attempts[0], &attempts[1]);
    ensure!(
        field(first, "/task/id")? == "worker" && field(second, "/task/id")? == "worker",
        "task identity"
    );
    for p in ["/task/created_ms", "/task/required_checks"] {
        ensure!(
            field(first, p)? == field(second, p)?,
            "task metadata changed"
        );
    }
    for p in ["/attempt/id", "/session/id"] {
        ensure!(
            field(first, p)? != field(second, p)?,
            "attempt/session not recreated"
        );
    }
    ensure!(
        field(second, "/launch/resume/previous_attempt")? == field(first, "/attempt/id")?
            && truthy(e.get("historical_launches")),
        "attempt history"
    );
    for row in array(e, "/dashboard/rows")? {
        ensure!(
            field(row, "/kind")? != "launch",
            "historical launch shown in dashboard"
        );
    }
    ensure!(
        field(second, "/launch/resume/native_session")? == field(e, "/session")?
            && field(e, "/resume_retry/session")? == field(second, "/session")?,
        "resume session identity"
    );
    ensure!(
        field(e, "/old_prompt_before")? == field(e, "/old_prompt_after")?
            && field(e, "/pending_before")? == field(e, "/pending_after")?
            && field(e, "/pending_after/receipt/bytes_written")? == 0,
        "pending/historical input changed"
    );
    ensure!(
        field(e, "/loss/attempt/state")? == "lost"
            && field(e, "/verified_task/outcome")? == "verified"
            && field(e, "/verified_task/attempt")? == field(second, "/attempt/id")?,
        "loss/verification mismatch"
    );
    ensure!(
        field(e, "/refused_cases")? == &json!(["metadata", "cancelled", "terminator"])
            && field(e, "/creation_requests")? == 1
            && field(e, "/dropped_reply")?
                .as_str()
                .context("dropped reply")?
                .contains("creation outcome uncertain"),
        "creation/rejection coverage"
    );
    let outcomes = array(e, "/zor_outcomes")?;
    ensure!(outcomes.len() == 2, "outcome count");
    for outcome in outcomes {
        ensure!(
            field(outcome, "/wait")? == "response-observed"
                && field(outcome, "/report_binding/message")?
                    == field(outcome, "/response/message")?
                && field(outcome, "/report_binding/message/session")? == field(e, "/session")?,
            "bound outcome identity"
        );
    }
    Ok(())
}
fn load(name: &str) -> Result<Value> {
    Ok(serde_json::from_slice(&fs::read(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .context("zor root")?
            .join("tests/fixtures/agents/opencode-1.18.29")
            .join(name),
    )?)?)
}
fn provenance(e: &Value, source: &str) -> Result<()> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .context("root")?;
    let recorded = field(e, "/harness_sha256")?
        .as_str()
        .context("harness hash")?;
    let historical = matches!(
        (source, recorded),
        (
            "capture_opencode_resume.py",
            "1ef0b4ae26a741e6206155d2bc9c4fd700e6274c9d3478ffaf725e53d9d75b00"
        ) | (
            "capture_zor_resume.py",
            "004f3f24f48b90f99334995e3720d411a89617beb4f3f3c2a6269364bc5eea0f"
        )
    );
    let harness = if historical {
        root.join("tools/archive").join(format!("{source}.txt"))
    } else {
        root.join("tools").join(source)
    };
    ensure!(
        field(e, "/opencode_sha256")? == field(&load("release.json")?, "/binary_sha256")?,
        "release binary"
    );
    ensure!(
        field(e, "/harness_sha256")?
            .as_str()
            .context("harness hash")?
            == crate::runtime::hash(&harness)?,
        "capture harness changed"
    );
    Ok(())
}
pub fn native_regressions() -> Result<()> {
    let e = load("resume.json")?;
    native(&e)?;
    provenance(&e, "capture_opencode_resume.py")?;
    for mutation in 0..8 {
        let mut v = e.clone();
        match mutation {
            0 => v["targets"][1]["instance"] = v["targets"][0]["instance"].clone(),
            1 => v["submissions"][1]["message"]["sessionID"] = "different".into(),
            2 => v["captures"][2]["input_sequence"] = 1.into(),
            3 => v["submissions"][1]["receipt"]["bytes_written"] = 1.into(),
            4 => {
                let chat = array(&v, "/events")?
                    .iter()
                    .find(|r| r["hook"] == "chat.message")
                    .context("chat")?
                    .clone();
                v["events"].as_array_mut().context("events")?.push(chat);
            }
            5 => {
                for messages in v["requests"].as_array_mut().context("requests")? {
                    for m in messages.as_array_mut().context("messages")? {
                        if m["role"] == "assistant" {
                            m["content"] = "no prior response".into();
                        }
                    }
                }
            }
            6 => {
                for row in v["events"].as_array_mut().context("events")? {
                    if row.pointer("/event/type") == Some(&json!("message.updated")) {
                        row["event"]["properties"]["info"]["parentID"] = "unrelated".into();
                    }
                }
            }
            _ => v["provider_stopped"] = false.into(),
        }
        ensure!(native(&v).is_err(), "native mutation {mutation} accepted");
    }
    let mut missing = e.clone();
    missing["events"]
        .as_array_mut()
        .context("events")?
        .iter_mut()
        .find(|r| r["hook"] == "chat.message")
        .context("chat")?["output"]["parts"]
        .as_array_mut()
        .context("parts")?
        .push(json!({}));
    ensure!(native(&missing).is_err(), "missing part type accepted");
    let mut missing = e.clone();
    missing["requests"][0][0]
        .as_object_mut()
        .context("first provider message")?
        .remove("role");
    ensure!(native(&missing).is_err(), "missing provider role accepted");
    println!("PASS native resume identity, provider history, receipts and no replay");
    Ok(())
}
pub fn zor_regressions() -> Result<()> {
    let e = load("zor-resume.json")?;
    provenance(&e, "capture_zor_resume.py")?;
    ensure!(field(&e, "/passed")? == true, "capture failed");
    let attempts = array(&e, "/zor_attempts")?;
    ensure!(attempts.len() == 2, "attempts");
    ensure!(
        field(&attempts[1], "/launch/resume/environment")?
            == field(&attempts[0], "/launch/integration/storage_environment")?,
        "storage environment changed"
    );
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .context("root")?;
    let plugin = crate::runtime::hash(&root.join("integrations/opencode.mjs"))?;
    ensure!(
        Path::new(
            field(&attempts[1], "/launch/integration/plugin")?
                .as_str()
                .context("plugin")?
        )
        .file_name()
        .context("plugin filename")?
            == format!("opencode-{plugin}.mjs").as_str(),
        "plugin hash"
    );
    zor(&e)?;
    for mutation in 0..9 {
        let mut v = e.clone();
        match mutation {
            0 => v["dashboard"]["rows"]
                .as_array_mut()
                .context("rows")?
                .push(json!({"kind":"launch","attention":true})),
            1 => v["creation_requests"] = 2.into(),
            2 => v["zor_attempts"][1]["task"]["id"] = "replacement-task".into(),
            3 => {
                v["zor_attempts"][1]["attempt"]["id"] =
                    v["zor_attempts"][0]["attempt"]["id"].clone()
            }
            4 => v["pending_after"]["receipt"]["bytes_written"] = 1.into(),
            5 => v["resume_retry"]["session"]["id"] = "different-session".into(),
            6 => v["zor_attempts"][1]["launch"]["resume"]["native_session"] = "foreign".into(),
            7 => v["verified_task"]["outcome"] = "open".into(),
            _ => v["refused_cases"] = json!([]),
        }
        ensure!(zor(&v).is_err(), "zor mutation {mutation} accepted");
    }
    println!(
        "PASS zor resume identity, durable pending input, retry, verification and historical-launch isolation"
    );
    Ok(())
}
#[cfg(test)]
mod tests {
    #[test]
    fn native() {
        super::native_regressions().unwrap();
    }
    #[test]
    fn zor() {
        super::zor_regressions().unwrap();
    }
}
