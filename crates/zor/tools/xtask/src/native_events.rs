//! Correlated native events must prove final text after an owned tool completes.
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{collections::BTreeSet, fs, path::Path};
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
    "Reply with FIXTURE RESPONSE",
    "ZOR_TOOL_STOP: inspect the fixture",
];
pub(super) fn validate(e: &Value) -> Result<Value> {
    let events = array(e, "/events")?;
    let submissions = array(e, "/submissions")?;
    let requests = array(e, "/requests")?;
    let mut chats = Vec::new();
    for (i, row) in events.iter().enumerate() {
        if field(row, "/hook")? == "chat.message" {
            chats.push((i, row));
        }
    }
    ensure!(
        chats.len() == 2 && submissions.len() == 2,
        "consumed prompts/submissions"
    );
    let operations = submissions
        .iter()
        .map(|s| field(s, "/receipt/operation"))
        .collect::<Result<Vec<_>>>()?;
    ensure!(operations[0] != operations[1], "operation reused");
    let ids = chats
        .iter()
        .map(|(_, r)| field(r, "/output/message/id"))
        .collect::<Result<Vec<_>>>()?;
    ensure!(ids[0] != ids[1], "user message reused");
    let session = field(chats[0].1, "/output/message/sessionID")?;
    let mut outcomes = Vec::new();
    for (turn, (chat_index, chat)) in chats.iter().enumerate() {
        let receipt = field(&submissions[turn], "/final_receipt")?;
        ensure!(
            field(receipt, "/operation")? == operations[turn]
                && field(receipt, "/state")? == "delivered"
                && field(receipt, "/input_sequence")? == turn + 1
                && field(receipt, "/bytes_written")? == PROMPTS[turn].len() + 1,
            "receipt mismatch/partial/intervening input"
        );
        ensure!(
            field(chat, "/output/message/sessionID")? == session,
            "session changed"
        );
        let mut parts = Vec::new();
        for p in array(chat, "/output/parts")? {
            if field(p, "/type")? == "text" {
                parts.push(field(p, "/text")?);
            }
        }
        ensure!(parts == vec![&json!(PROMPTS[turn])], "prompt parts");
        let mut completed = Vec::new();
        let mut tool_messages = BTreeSet::new();
        for (i, row) in events.iter().enumerate() {
            match row.pointer("/event/type").and_then(Value::as_str) {
                Some("message.updated") => {
                    let info = field(row, "/event/properties/info")?;
                    if info.get("role") == Some(&json!("assistant"))
                        && info.get("parentID") == Some(ids[turn])
                        && info.get("sessionID") == Some(session)
                        && truthy(info.pointer("/time/completed"))
                        && info.get("finish") == Some(&json!("stop"))
                        && !truthy(info.get("error"))
                    {
                        completed.push((i, info));
                    }
                }
                Some("message.part.updated") => {
                    let part = field(row, "/event/properties/part")?;
                    if part.get("type") == Some(&json!("tool"))
                        && part.get("sessionID") == Some(session)
                    {
                        tool_messages.insert(
                            field(part, "/messageID")?
                                .as_str()
                                .context("tool message")?,
                        );
                    }
                }
                _ => {}
            }
        }
        ensure!(
            completed.first().is_some_and(|(i, _)| i > chat_index),
            "missing correlated response"
        );
        let last = completed.last().context("completion")?;
        let mut idle = Vec::new();
        for (i, row) in events.iter().enumerate() {
            if row.pointer("/event/type") == Some(&json!("session.idle"))
                && field(row, "/event/properties/sessionID")? == session
                && i > last.0
            {
                idle.push(i);
            }
        }
        let idle = *idle.first().context("missing later idle")?;
        if turn == 0 {
            ensure!(idle < chats[1].0, "second prompt preceded idle");
        }
        let completed_ids = completed
            .iter()
            .map(|(_, m)| field(m, "/id")?.as_str().context("completed id"))
            .collect::<Result<Vec<_>>>()?;
        if turn == 1 {
            ensure!(
                completed_ids.iter().collect::<BTreeSet<_>>().len() >= 2
                    && tool_messages.contains(completed_ids[0])
                    && !tool_messages.contains(completed_ids.last().context("final id")?),
                "tool stop lacks final continuation"
            );
            let mut read = false;
            for row in events {
                if row.pointer("/event/type") == Some(&json!("message.part.updated")) {
                    let part = field(row, "/event/properties/part")?;
                    read |= part.get("messageID") == Some(&json!(completed_ids[0]))
                        && part.get("sessionID") == Some(session)
                        && part.get("type") == Some(&json!("tool"))
                        && part.get("tool") == Some(&json!("read"))
                        && part.pointer("/state/status") == Some(&json!("completed"))
                        && part.pointer("/state/input/filePath")
                            == Some(&json!("<FIXTURE_ROOT>/work/fixture.txt"))
                        && part
                            .pointer("/state/output")
                            .and_then(Value::as_str)
                            .is_some_and(|s| s.contains("Owned fixture."));
                }
            }
            ensure!(read, "owned fixture read did not complete");
        }
        let final_id = completed_ids.last().context("final id")?;
        let mut text = false;
        for row in events {
            if row.pointer("/event/type") == Some(&json!("message.part.updated")) {
                let part = field(row, "/event/properties/part")?;
                text |= part.get("messageID") == Some(&json!(final_id))
                    && part.get("sessionID") == Some(session)
                    && part.get("type") == Some(&json!("text"))
                    && part.get("text") == Some(&json!("FIXTURE RESPONSE"));
            }
        }
        ensure!(text, "missing correlated final text");
        outcomes.push(json!({"user_message":ids[turn],"session":session,"chat_index":chat_index,"input_message_id":field(chat,"/input")?.get("messageID"),"completed_stop_messages":completed_ids,"completed_indices":completed.iter().map(|(i,_)|i).collect::<Vec<_>>(),"idle_index":idle}));
    }
    let mut responses = Vec::new();
    for r in requests {
        responses.push(field(r, "/response")?);
    }
    ensure!(
        responses.contains(&&json!("tool-stop")) && responses.contains(&&json!("after-tool")),
        "provider tool loop missing"
    );
    ensure!(
        !events
            .iter()
            .any(|r| r.pointer("/event/type") == Some(&json!("session.error"))),
        "session error"
    );
    Ok(Value::Array(outcomes))
}
pub fn regressions() -> Result<()> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .context("root")?;
    let e: Value = serde_json::from_slice(&fs::read(
        root.join("tests/fixtures/agents/opencode-1.18.29/events.json"),
    )?)?;
    ensure!(
        validate(&e)? == *field(&e, "/outcomes")?,
        "recorded outcomes mismatch"
    );
    for mutation in 0..12 {
        let mut v = e.clone();
        let final_id = array(&e["outcomes"][1], "/completed_stop_messages")?
            .last()
            .context("final")?
            .clone();
        if mutation == 1 {
            v["events"]
                .as_array_mut()
                .context("events")?
                .retain(|r| r.pointer("/event/properties/info/id") != Some(&final_id));
        } else if (5..=7).contains(&mutation) {
            let (key, value) = match mutation {
                5 => ("input_sequence", json!(3)),
                6 => ("bytes_written", json!(1)),
                _ => ("state", json!("queued")),
            };
            v["submissions"][1]["final_receipt"][key] = value;
        } else {
            for row in v["events"].as_array_mut().context("events")? {
                if mutation == 0 {
                    if let Some(info) = row.pointer_mut("/event/properties/info")
                        && info.get("parentID") == Some(&e["outcomes"][1]["user_message"])
                    {
                        info["parentID"] = e["outcomes"][0]["user_message"].clone();
                    }
                    continue;
                }
                if let Some(part) = row.pointer_mut("/event/properties/part") {
                    let kind = part
                        .get("type")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_owned();
                    match mutation {
                        2 | 3 if part.get("messageID") == Some(&final_id) && kind == "text" => {
                            if mutation == 2 {
                                part["text"] = "".into();
                            } else {
                                part["messageID"] = "unrelated".into();
                            }
                        }
                        4 if kind == "tool" => part["state"]["status"] = "error".into(),
                        8 if kind == "tool" => part["tool"] = "unrelated".into(),
                        9 if kind == "tool" => {
                            part["state"]["input"]["filePath"] = "/unrelated".into()
                        }
                        10 if kind == "tool" => part["sessionID"] = "unrelated".into(),
                        11 if kind == "text" => part["sessionID"] = "unrelated".into(),
                        _ => {}
                    }
                }
            }
        }
        ensure!(
            validate(&v).is_err(),
            "native event mutation {mutation} accepted"
        );
    }
    println!(
        "PASS native event correlation, owned tool completion, final text and receipt mutations"
    );
    Ok(())
}
#[cfg(test)]
mod tests {
    #[test]
    fn retained_and_negative_cases() {
        super::regressions().unwrap();
    }
}
