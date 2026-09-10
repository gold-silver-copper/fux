//! Retained managed integration contract, distinct from provider or task success.
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{collections::BTreeSet, fs, path::Path};
fn f<'a>(v: &'a Value, p: &str) -> Result<&'a Value> {
    v.pointer(p).with_context(|| format!("missing {p}"))
}
fn a<'a>(v: &'a Value, p: &str) -> Result<&'a Vec<Value>> {
    f(v, p)?.as_array().context("array")
}
fn n(v: &Value, p: &str) -> Result<f64> {
    f(v, p)?.as_f64().context("number")
}
fn eq(v: &Value, p: &str, want: Value) -> Result<()> {
    ensure!(f(v, p)? == &want, "mismatch {p}");
    Ok(())
}
fn same(v: &Value, left: &str, right: &str) -> Result<()> {
    ensure!(f(v, left)? == f(v, right)?, "mismatch {left} / {right}");
    Ok(())
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
fn kind(row: &Value) -> Option<&str> {
    row.pointer("/event/type").and_then(Value::as_str)
}
pub(super) fn questions() -> Value {
    json!([{"question":"Which fixture option?","header":"Fixture","options":[{"label":"Alpha","description":"Use the first fixture option"},{"label":"Beta","description":"Use the second fixture option"}]}])
}
pub(super) fn validate(e: &Value) -> Result<()> {
    let all = a(e, "/events")?;
    let outcomes = a(e, "/outcomes")?;
    let storage = truthy(e.get("storage_metadata"));
    let registration = f(e, "/registered/launch/integration")?;
    if storage {
        eq(
            registration,
            "/storage_environment",
            json!({"HOME":"<FIXTURE_ROOT>","XDG_CONFIG_HOME":"<FIXTURE_ROOT>/config","XDG_DATA_HOME":"<FIXTURE_ROOT>/data","XDG_STATE_HOME":"<FIXTURE_ROOT>/state","XDG_CACHE_HOME":"<FIXTURE_ROOT>/cache"}),
        )?;
    }
    let blocker = e
        .get("blocker")
        .map(|v| v.as_str().context("blocker"))
        .transpose()?
        .unwrap_or("permission");
    ensure!(
        ["permission", "question"].contains(&blocker),
        "unknown blocker"
    );
    let prompts = [
        "Reply with FIXTURE RESPONSE",
        "ZOR_TOOL_STOP: inspect the fixture",
        if blocker == "question" {
            "ZOR_QUESTION: ask which fixture option to use"
        } else {
            "ZOR_APPROVAL: request a harmless command"
        },
    ];
    let boundary = if let Some(b) = e.get("observation_event_count") {
        usize::try_from(b.as_u64().context("integer observation boundary")?)?
    } else {
        ensure!(blocker != "question", "question boundary missing");
        all.len()
    };
    ensure!(
        boundary > 0 && boundary <= all.len(),
        "observation boundary"
    );
    let events = &all[..boundary];
    ensure!(
        !all.iter()
            .any(|r| matches!(kind(r), Some("permission.replied" | "question.replied"))),
        "blocker answered"
    );
    let retirement = f(e, "/retirement")?;
    let retired = f(retirement, "/prompt")?;
    for (p, w) in [
        ("/arm_status", json!("armed")),
        ("/late_arm_status", json!("rejected")),
        ("/input_before", json!(0)),
        ("/input_after", json!(0)),
    ] {
        eq(retirement, p, w)?;
    }
    for (p, w) in [
        ("/id", json!("retired-before-input")),
        ("/released", json!(true)),
        ("/wait", json!("cancelled")),
        ("/delivery", json!("reserved")),
        ("/receipt/bytes_written", json!(0)),
        ("/arm/acknowledged", json!(false)),
        ("/arm/input_started", json!(false)),
        ("/arm/disarm_requested", json!(true)),
        ("/arm/disarmed", json!(true)),
        ("/response", Value::Null),
        ("/report_binding", Value::Null),
    ] {
        eq(retired, p, w)?;
    }
    same(retired, "/arm/input_operation", "/receipt/operation")?;
    ensure!(
        f(retired, "/arm/producer")? == f(registration, "/producer")?,
        "retired producer"
    );
    let mut chats = Vec::new();
    for (i, row) in events.iter().enumerate() {
        if f(row, "/hook")? == "chat.message" {
            chats.push((i, row));
        }
    }
    ensure!(
        chats.len() == 3 && outcomes.len() == 3,
        "managed prompt count"
    );
    eq(e, "/fux_exit", json!(0))?;
    ensure!(truthy(e.get("provider_thread_stopped")), "provider cleanup");
    eq(e, "/task_outcome", json!("open"))?;
    let producer = f(registration, "/producer")?;
    let mut producers = vec![producer; 3];
    let reload = e.get("producer_reload").filter(|v| truthy(Some(v)));
    if let Some(reload) = reload {
        eq(
            e,
            "/reload_plugin_sha256",
            json!(format!(
                "{:x}",
                Sha256::digest(include_bytes!("integration/reload-plugin.js.txt"))
            )),
        )?;
        if e.get("reload_exec_kind") == Some(&json!("rust-binary")) {
            eq(e, "/harness_kind", json!("rust-binary"))?;
            same(e, "/reload_exec_sha256", "/harness_sha256")?;
        } else {
            eq(
                e,
                "/reload_exec_sha256",
                json!(format!(
                    "{:x}",
                    Sha256::digest(include_bytes!("integration/reload-exec.py.txt"))
                )),
            )?;
        }
        ensure!(
            blocker == "question"
                && f(reload, "/before/producer")? == producer
                && f(reload, "/after/producer")? != producer,
            "producer not replaced"
        );
        eq(
            reload,
            "/after/retired",
            json!([{"producer":producer,"registered_ms":f(reload,"/before/registered_ms")?,"retired_ms":f(reload,"/after/registered_ms")?}]),
        )?;
        eq(reload, "/input_before", json!(1))?;
        eq(reload, "/input_after", json!(1))?;
        same(reload, "/target_before", "/target_after")?;
        ensure!(
            f(reload, "/target_before")? == f(e, "/registered/session/target")?,
            "worker replaced"
        );
        if storage {
            same(
                reload,
                "/after/storage_environment",
                "/before/storage_environment",
            )?;
            ensure!(
                f(reload, "/after/storage_environment")?
                    == f(registration, "/storage_environment")?,
                "storage namespace changed"
            );
        }
        eq(reload, "/hook_before/generation", json!(1))?;
        eq(reload, "/hook_after/generation", json!(2))?;
        same(reload, "/hook_before/pid", "/hook_after/pid")?;
        ensure!(
            f(reload, "/retained_response_after")? == f(&outcomes[0], "/response")?,
            "old response rewritten"
        );
        producers = vec![
            producer,
            f(reload, "/after/producer")?,
            f(reload, "/after/producer")?,
        ];
    }
    if blocker == "question" {
        for probe in a(e, "/adapter_status")? {
            f(probe, "/heartbeat")?;
        }
    }
    if e.get("adapter_status").is_some() {
        let probes = a(e, "/adapter_status")?;
        ensure!(probes.len() == 2, "probe count");
        for (i, probe) in probes.iter().enumerate() {
            for (p, w) in [
                ("/availability", json!("reachable")),
                ("/stage", json!("complete")),
                ("/scope", json!("point-in-time-endpoint-probe")),
                ("/task_id", json!("integrated")),
            ] {
                eq(probe, p, w)?;
            }
            ensure!(
                f(probe, "/producer")? == producers[if i == 0 { 0 } else { 2 }],
                "probe producer"
            );
            same(probe, "/generation", "/current_generation")?;
            if let Some(h) = probe.get("heartbeat") {
                eq(h, "/status", json!("current"))?;
                eq(h, "/scope", json!("producer-freshness-only"))?;
                eq(h, "/ttl_ms", json!(6000))?;
                ensure!(
                    n(h, "/sequence")? > 0.
                        && n(h, "/age_ms")? >= 0.
                        && n(h, "/age_ms")? < n(h, "/ttl_ms")?
                        && n(h, "/received_ms")? >= n(probe, "/registered_ms")?,
                    "heartbeat freshness"
                );
            }
        }
        if let Some(h) = probes[1].get("heartbeat") {
            ensure!(
                n(h, "/received_ms")? > n(&outcomes[2], "/response/received_ms")?,
                "heartbeat predates blocker"
            );
        }
    }
    let mut operations = BTreeSet::new();
    let mut messages = BTreeSet::new();
    for (i, (_, chat)) in chats.iter().enumerate() {
        let o = &outcomes[i];
        let binding = f(o, "/report_binding")?;
        let response = f(o, "/response")?;
        let receipt = f(o, "/receipt")?;
        let native = json!({"session":f(chat,"/output/message/sessionID")?,"id":f(chat,"/output/message/id")?});
        ensure!(
            f(chat, "/input/sessionID")? == f(&native, "/session")?,
            "native input session"
        );
        let parts = a(chat, "/output/parts")?;
        ensure!(
            parts.len() == 1 && f(&parts[0], "/text")? == prompts[i],
            "native prompt text"
        );
        ensure!(
            f(binding, "/message")? == &native && f(response, "/message")? == &native,
            "report ancestry"
        );
        for v in [binding, response, f(o, "/arm")?] {
            ensure!(f(v, "/producer")? == producers[i], "producer ancestry");
            ensure!(
                f(v, "/input_operation")? == f(receipt, "/operation")?,
                "operation ancestry"
            );
        }
        for (p, w) in [
            ("/arm/acknowledged", json!(true)),
            ("/arm/input_started", json!(true)),
            ("/arm/disarm_requested", json!(false)),
            ("/arm/disarmed", json!(false)),
            ("/delivery", json!("delivered")),
        ] {
            eq(o, p, w)?;
        }
        let offset = if reload.is_some() && i > 0 { i - 1 } else { i };
        eq(binding, "/sequence", json!(2 * offset + 1))?;
        eq(response, "/sequence", json!(2 * offset + 2))?;
        eq(receipt, "/input_sequence", json!(i + 1))?;
        eq(receipt, "/bytes_written", json!(prompts[i].len() + 1))?;
        let expected = if i == 2 {
            "needs-input"
        } else {
            "response-observed"
        };
        eq(o, "/wait", json!(expected))?;
        eq(response, "/kind", json!(expected))?;
        operations.insert(f(receipt, "/operation")?.to_string());
        messages.insert(f(&native, "/id")?.to_string());
    }
    ensure!(
        operations.len() == 3 && messages.len() == 3,
        "reused identity"
    );
    eq(e, "/capture/input_sequence", json!(3))?;
    if blocker == "question" {
        let heartbeat = f(e, "/adapter_status/1/heartbeat")?;
        let last = &outcomes[2];
        let claim = json!({"state":"blocked","operation":f(last,"/id")?,"input_operation":f(last,"/receipt/operation")?,"message":f(last,"/report_binding/message")?});
        eq(heartbeat, "/observation", claim.clone())?;
        eq(heartbeat, "/input_sequence", json!(3))?;
        let dashboard = f(e, "/dashboard")?;
        eq(e, "/zor_service_exit", json!(0))?;
        eq(dashboard, "/stale", json!(false))?;
        let target = f(e, "/registered/session/target")?;
        let mut rows = Vec::new();
        for r in a(dashboard, "/rows")? {
            if f(r, "/kind")? == "observation" && f(r, "/target")? == target {
                rows.push(r);
            }
        }
        ensure!(rows.len() == 1, "native dashboard target");
        let row = rows[0];
        eq(row, "/status", json!("blocked"))?;
        eq(row, "/attention", json!(true))?;
        eq(row, "/task_outcome", Value::Null)?;
        ensure!(
            n(row, "/age_upper_bound_ms")? >= 0. && n(row, "/age_upper_bound_ms")? <= 5000.,
            "stale dashboard row"
        );
        let observed = f(row, "/evidence")?;
        for (p, w) in [
            ("/source", json!("integration")),
            ("/claim", claim),
            ("/fresh", json!(true)),
            ("/correlated", json!(true)),
            ("/heartbeat_status", json!("current")),
            ("/scope", json!("agent-observation; not-task-completion")),
        ] {
            eq(observed, p, w)?;
        }
        ensure!(
            f(observed, "/producer")? == producers[2]
                && n(observed, "/heartbeat_sequence")? >= n(heartbeat, "/sequence")?
                && n(observed, "/heartbeat_age_upper_bound_ms")? >= 0.
                && n(observed, "/heartbeat_age_upper_bound_ms")? < 6000.,
            "dashboard heartbeat"
        );
        let mut tasks = Vec::new();
        for row in a(dashboard, "/rows")? {
            if f(row, "/key")? == "task:integrated" {
                tasks.push(row);
            }
        }
        ensure!(tasks.len() == 1, "dashboard task");
        eq(tasks[0], "/task_outcome", json!("open"))?;
        eq(tasks[0], "/status", json!("needs-input"))?;
    }
    let mut submissions = Vec::new();
    for o in &outcomes[..2] {
        let receipt = f(o, "/receipt")?.clone();
        let mut final_receipt = receipt.clone();
        final_receipt["state"] = f(o, "/delivery")?.clone();
        submissions.push(json!({"receipt":receipt,"final_receipt":final_receipt}));
    }
    crate::native_events::validate(
        &json!({"events":events[..chats[2].0],"submissions":submissions,"requests":f(e,"/requests")?}),
    )?;
    let native = f(&outcomes[2], "/report_binding/message")?;
    let later = &events[chats[2].0 + 1..];
    let mut assistants = BTreeSet::new();
    let mut blockers = Vec::new();
    for row in later {
        if kind(row) == Some("message.updated") {
            let info = f(row, "/event/properties/info")?;
            if info.get("role") == Some(&json!("assistant"))
                && info.get("parentID") == Some(f(native, "/id")?)
                && info.get("sessionID") == Some(f(native, "/session")?)
            {
                assistants.insert(f(info, "/id")?.to_string());
            }
        }
        if kind(row) == Some(&format!("{blocker}.asked")) {
            blockers.push(f(row, "/event/properties")?);
        }
    }
    ensure!(blockers.len() == 1, "root blocker count");
    let request = blockers[0];
    ensure!(
        f(request, "/sessionID")? == f(native, "/session")?
            && assistants.contains(&f(request, "/tool/messageID")?.to_string()),
        "foreign blocker ancestry"
    );
    if blocker == "permission" {
        eq(request, "/permission", json!("bash"))?;
        eq(request, "/metadata/command", json!("printf harmless"))?;
        eq(request, "/tool/callID", json!("fixture-approval"))?;
    } else {
        eq(request, "/tool/callID", json!("fixture-question"))?;
        eq(request, "/questions", questions())?;
        let mut provider_question = false;
        for row in a(e, "/requests")? {
            if f(row, "/response")? == "question" {
                provider_question = true;
                break;
            }
        }
        ensure!(provider_question, "provider question missing");
        let mut running = false;
        for row in later {
            if kind(row) == Some("message.part.updated") {
                let part = f(row, "/event/properties/part")?;
                running |= part.get("type") == Some(&json!("tool"))
                    && part.get("tool") == Some(&json!("question"))
                    && part.get("callID") == Some(f(request, "/tool/callID")?)
                    && part.get("messageID") == Some(f(request, "/tool/messageID")?)
                    && part.get("sessionID") == Some(f(native, "/session")?)
                    && part.pointer("/state/status") == Some(&json!("running"))
                    && part.pointer("/state/input") == Some(&json!({"questions":questions()}));
            }
        }
        ensure!(running, "running question evidence missing");
    }
    ensure!(
        !events.iter().any(|r| matches!(
            kind(r),
            Some("permission.replied" | "question.replied" | "question.rejected" | "session.error")
        )),
        "blocker ended before observation"
    );
    for row in events {
        if kind(row) == Some("message.part.updated") {
            let part = f(row, "/event/properties/part")?;
            ensure!(
                !(part
                    .get("messageID")
                    .is_some_and(|id| assistants.contains(&id.to_string()))
                    && part.get("type") == Some(&json!("tool"))
                    && matches!(
                        part.pointer("/state/status").and_then(Value::as_str),
                        Some("completed" | "error")
                    )),
                "blocked tool ended"
            );
        }
    }
    ensure!(
        f(e, "/capture/text")?
            .as_str()
            .context("capture")?
            .contains(if blocker == "permission" {
                "Permission required"
            } else {
                "Which fixture option?"
            }),
        "blocker UI missing"
    );
    Ok(())
}
fn reject(e: &Value, name: &str, mutate: impl FnOnce(&mut Value)) -> Result<()> {
    let mut changed = e.clone();
    mutate(&mut changed);
    ensure!(validate(&changed).is_err(), "accepted mutation: {name}");
    Ok(())
}
fn change(e: &Value, path: &str, value: Value) -> Result<()> {
    reject(e, path, |e| {
        *e.pointer_mut(path).expect("mutation path") = value
    })
}
fn remove(e: &Value, path: &str, key: &str) -> Result<()> {
    reject(e, key, |e| {
        e.pointer_mut(path)
            .unwrap()
            .as_object_mut()
            .unwrap()
            .remove(key)
            .expect("mutation key");
    })
}
fn event_mut<'a>(e: &'a mut Value, event: &str) -> &'a mut Value {
    e["events"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|r| kind(r) == Some(event))
        .unwrap()
        .pointer_mut("/event/properties")
        .unwrap()
}
fn observed_mut(e: &mut Value) -> &mut Value {
    e["dashboard"]["rows"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|r| r["kind"] == "observation" && r["evidence"]["source"] == "integration")
        .unwrap()
}
fn insert_boundary(e: &mut Value, row: Value) {
    let i = e["observation_event_count"].as_u64().unwrap() as usize;
    e["events"].as_array_mut().unwrap().insert(i, row);
    e["observation_event_count"] = json!(i + 1);
}
fn question_regressions(e: &Value) -> Result<()> {
    ensure!(a(e, "/adapter_status")?.len() == 2, "probe count");
    for probe in a(e, "/adapter_status")? {
        eq(probe, "/heartbeat/status", json!("current"))?;
    }
    for (p, v) in [
        ("/adapter_status/1/heartbeat/status", json!("expired")),
        ("/adapter_status/0/heartbeat/age_ms", json!(6000)),
        ("/adapter_status/1/heartbeat/sequence", json!(0)),
        ("/adapter_status/0/heartbeat/received_ms", json!(0)),
        (
            "/adapter_status/1/heartbeat/received_ms",
            e["outcomes"][2]["response"]["received_ms"].clone(),
        ),
        (
            "/adapter_status/1/heartbeat/observation/message/id",
            json!("foreign"),
        ),
        ("/adapter_status/1/heartbeat/input_sequence", json!(2)),
        ("/dashboard/stale", json!(true)),
        ("/zor_service_exit", json!(1)),
        ("/adapter_status/0/producer", json!("other-producer")),
        ("/adapter_status/1/current_generation", json!(0)),
        ("/adapter_status/1/availability", json!("unavailable")),
        ("/capture/text", json!("")),
        (
            "/outcomes/2/response/message",
            json!({"session":"other","id":"other"}),
        ),
    ] {
        change(e, p, v)?;
    }
    reject(e, "missing observation row key", |e| {
        observed_mut(e)
            .as_object_mut()
            .unwrap()
            .remove("key")
            .unwrap();
    })?;
    reject(e, "missing provider response field", |e| {
        e["requests"][0]
            .as_object_mut()
            .unwrap()
            .remove("response")
            .unwrap();
    })?;
    remove(e, "/adapter_status/1", "heartbeat")?;
    remove(e, "/adapter_status/1/heartbeat", "observation")?;
    remove(e, "", "observation_event_count")?;
    for (p, v) in [
        ("/evidence/source", json!("passive")),
        ("/target/pid", json!(0)),
        ("/evidence/fresh", json!(false)),
        ("/evidence/correlated", json!(false)),
        ("/age_upper_bound_ms", json!(5001)),
        ("/evidence/claim/operation", json!("older-prompt")),
        ("/task_outcome", json!("verified")),
    ] {
        reject(e, p, |e| *observed_mut(e).pointer_mut(p).unwrap() = v)?;
    }
    for (p, v) in [
        ("/sessionID", json!("child-session")),
        ("/tool/messageID", json!("foreign")),
        ("/tool/callID", json!("foreign")),
        ("/questions/0/question", json!("different question")),
    ] {
        reject(e, p, |e| {
            *event_mut(e, "question.asked").pointer_mut(p).unwrap() = v
        })?;
    }
    reject(e, "question without tool", |e| {
        event_mut(e, "question.asked")
            .as_object_mut()
            .unwrap()
            .remove("tool");
    })?;
    for event in ["question.replied", "question.rejected"] {
        reject(e, event, |e| {
            insert_boundary(e, json!({"hook":"event","event":{"type":event}}))
        })?;
    }
    let mut shutdown = e.clone();
    shutdown["events"]
        .as_array_mut()
        .unwrap()
        .push(json!({"hook":"event","event":{"type":"question.rejected"}}));
    validate(&shutdown).context("post-boundary shutdown rejection is valid")?;
    for status in ["pending", "completed"] {
        reject(e, status, |e| {
            for row in e["events"].as_array_mut().unwrap() {
                if row.pointer("/event/properties/part/tool") == Some(&json!("question")) {
                    row["event"]["properties"]["part"]["state"]["status"] = json!(status);
                }
            }
        })?;
    }
    reject(e, "question error after running", |e| {
        let mut row = e["events"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| {
                r.pointer("/event/properties/part/tool") == Some(&json!("question"))
                    && r.pointer("/event/properties/part/state/status") == Some(&json!("running"))
            })
            .unwrap()
            .clone();
        row["event"]["properties"]["part"]["state"]["status"] = json!("error");
        insert_boundary(e, row);
    })?;
    Ok(())
}
pub fn regressions() -> Result<()> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .context("zor root")?
        .join("tests/fixtures/agents/opencode-1.18.29");
    for name in [
        "integration.json",
        "question.json",
        "producer-reload.json",
        "integration-storage.json",
    ] {
        let e: Value = serde_json::from_slice(&fs::read(root.join(name))?)?;
        validate(&e).with_context(|| format!("retained {name}"))?;
        match name {
            "integration.json" => {
                for (p, v) in [
                    ("/outcomes/0/report_binding/message/id", json!("foreign")),
                    ("/outcomes/1/receipt/bytes_written", json!(1)),
                    ("/outcomes/0/arm/acknowledged", json!(false)),
                    ("/outcomes/0/arm/input_started", json!(false)),
                    ("/outcomes/0/arm/disarm_requested", json!(true)),
                    ("/retirement/prompt/receipt/bytes_written", json!(1)),
                    ("/retirement/late_arm_status", json!("armed")),
                    ("/retirement/prompt/arm/input_started", json!(true)),
                    ("/provider_thread_stopped", json!(false)),
                ] {
                    change(&e, p, v)?;
                }
                reject(&e, "foreign permission parent", |e| {
                    event_mut(e, "permission.asked")["tool"]["messageID"] = json!("foreign")
                })?;
                reject(&e, "permission answered", |e| {
                    e["events"]
                        .as_array_mut()
                        .unwrap()
                        .push(json!({"hook":"event","event":{"type":"permission.replied"}}))
                })?;
                for text in [false, true] {
                    reject(
                        &e,
                        if text {
                            "missing final text"
                        } else {
                            "read tool failed"
                        },
                        |e| {
                            for row in e["events"].as_array_mut().unwrap() {
                                if let Some(part) = row.pointer_mut("/event/properties/part") {
                                    if text && part["type"] == "text" {
                                        part["text"] = json!("");
                                    }
                                    if !text && part["tool"] == "read" {
                                        part["state"]["status"] = json!("error");
                                    }
                                }
                            }
                        },
                    )?;
                }
            }
            "question.json" | "producer-reload.json" => {
                question_regressions(&e)?;
                if name == "producer-reload.json" {
                    for (p, v) in [
                        (
                            "/producer_reload/after/producer",
                            e["producer_reload"]["before"]["producer"].clone(),
                        ),
                        ("/producer_reload/target_after/pid", json!(0)),
                        ("/producer_reload/hook_after/pid", json!(0)),
                        ("/producer_reload/input_after", json!(2)),
                        ("/producer_reload/after/retired", json!([])),
                        (
                            "/producer_reload/retained_response_after/producer",
                            json!("new"),
                        ),
                        (
                            "/outcomes/1/report_binding/producer",
                            e["producer_reload"]["before"]["producer"].clone(),
                        ),
                        ("/reload_plugin_sha256", json!("bad")),
                        ("/reload_exec_sha256", json!("bad")),
                    ] {
                        change(&e, p, v)?;
                    }
                }
            }
            "integration-storage.json" => {
                eq(&e, "/storage_metadata", json!(true))?;
                for v in [Value::Null, json!({}), json!({"HOME":"/foreign"})] {
                    change(&e, "/registered/launch/integration/storage_environment", v)?;
                }
                change(
                    &e,
                    "/producer_reload/after/storage_environment",
                    Value::Null,
                )?;
            }
            _ => unreachable!(),
        }
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    #[test]
    fn retained_contracts_and_original_mutations() -> anyhow::Result<()> {
        super::regressions()
    }
}
