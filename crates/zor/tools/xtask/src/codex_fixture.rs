//! Account-free app-server protocol fixture for the production native worker.
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::io::{BufRead, Write};

fn emit(value: Value) -> Result<()> {
    let mut output = std::io::stdout().lock();
    serde_json::to_writer(&mut output, &value)?;
    output.write_all(b"\n")?;
    output.flush()?;
    Ok(())
}
fn main() -> Result<()> {
    let marker = std::env::args()
        .nth(1)
        .context("fixture receipt path missing")?;
    let exit_after_turn = std::env::args().any(|arg| arg == "--exit-after-turn");
    let workflow = std::env::args().any(|arg| arg == "--workflow");
    let controls = workflow || std::env::args().any(|arg| arg == "--controls");
    let mut retained_turn = Value::Null;
    let mut resumed = false;
    let previous_pid = std::fs::read_to_string(format!("{marker}.pid")).ok();
    std::fs::write(format!("{marker}.pid"), std::process::id().to_string())?;
    for line in std::io::stdin().lock().lines() {
        let frame: Value = serde_json::from_str(&line?)?;
        let id = frame.get("id").cloned().unwrap_or(Value::Null);
        match frame
            .get("method")
            .and_then(Value::as_str)
            .context("fixture method")?
        {
            "initialize" => emit(json!({"id":id,"result":{}}))?,
            "initialized" => {}
            "thread/resume" => {
                ensure!(
                    frame.pointer("/params/threadId") == Some(&json!("fixture-thread")),
                    "resume identity"
                );
                ensure!(
                    frame.pointer("/params/approvalPolicy") == Some(&json!("never")),
                    "interactive resume policy"
                );
                let previous_pid: i32 = previous_pid
                    .as_ref()
                    .context("previous provider PID")?
                    .parse()?;
                ensure!(
                    matches!(
                        nix::sys::signal::kill(nix::unistd::Pid::from_raw(previous_pid), None),
                        Err(nix::errno::Errno::ESRCH)
                    ),
                    "previous provider is still alive during resume"
                );
                retained_turn =
                    serde_json::from_slice(&std::fs::read(format!("{marker}.thread.json"))?)?;
                let mut receipt = std::fs::OpenOptions::new()
                    .create_new(true)
                    .write(true)
                    .open(format!("{marker}.resumed"))?;
                receipt.write_all(std::process::id().to_string().as_bytes())?;
                receipt.sync_all()?;
                resumed = true;
                emit(
                    json!({"id":id,"result":{"thread":{"id":"fixture-thread","cwd":std::env::current_dir()?,"ephemeral":false,"path":format!("{marker}.thread.json")}}}),
                )?;
            }
            "thread/start" => {
                ensure!(
                    frame.pointer("/params/approvalPolicy") == Some(&json!("never")),
                    "interactive policy"
                );
                emit(
                    json!({"id":id,"result":{"thread":{"id":"fixture-thread","cwd":std::env::current_dir()?,"ephemeral":false,"path":format!("{marker}.thread.json")}}}),
                )?;
                let marker = marker.clone();
                std::thread::spawn(move || -> Result<()> {
                    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
                    while !std::path::Path::new(&format!("{marker}.notify")).exists() {
                        ensure!(
                            std::time::Instant::now() < deadline,
                            "fixture notification trigger missing"
                        );
                        std::thread::sleep(std::time::Duration::from_millis(5));
                    }
                    emit(
                        json!({"method":"thread/status/changed","params":{"threadId":"fixture-thread"}}),
                    )?;
                    std::fs::write(format!("{marker}.notified"), b"sent")?;
                    Ok(())
                });
            }
            "turn/start" => {
                let text = frame
                    .pointer("/params/input/0/text")
                    .and_then(Value::as_str)
                    .context("literal text")?;
                let client = frame
                    .pointer("/params/clientUserMessageId")
                    .context("client identity")?;
                let mut receipt = std::fs::OpenOptions::new()
                    .create_new(true)
                    .write(true)
                    .open(&marker)
                    .context("duplicate native submission")?;
                receipt.write_all(text.as_bytes())?;
                receipt.sync_all()?;
                if workflow {
                    std::fs::write("output.txt", text.as_bytes())?;
                }
                let user = json!({"type":"userMessage","id":"fixture-user","clientId":client,"content":[{"type":"text","text":text}]});
                retained_turn =
                    json!({"id":"fixture-turn","status":"inProgress","items":[user.clone()]});
                std::fs::write(
                    format!("{marker}.thread.json"),
                    serde_json::to_vec(&retained_turn)?,
                )?;
                if controls {
                    // Deliberately lose the real acknowledgement and user event.
                    // A bare unrequested read response must not recover authority.
                    emit(
                        json!({"id":format!("read:{}", client.as_str().context("client")?),"result":{"thread":{"id":"fixture-thread","turns":[retained_turn.clone()]}}}),
                    )?;
                    continue;
                }
                emit(
                    json!({"id":id,"result":{"turn":{"id":"fixture-turn","status":"inProgress","items":[user.clone()]}}}),
                )?;
                emit(
                    json!({"method":"item/completed","params":{"threadId":"fixture-thread","turnId":"fixture-turn","item":user.clone()}}),
                )?;
                emit(
                    json!({"method":"turn/completed","params":{"threadId":"fixture-thread","turn":{"id":"fixture-turn","status":"completed","items":[user,{"type":"agentMessage","id":"fixture-answer","text":"native fixture response"}]}}}),
                )?;
            }
            "thread/read" if controls => {
                std::fs::OpenOptions::new()
                    .create_new(true)
                    .write(true)
                    .open(format!(
                        "{marker}.{}",
                        if resumed { "resumed-read" } else { "read" }
                    ))?;
                ensure!(
                    frame.pointer("/params/threadId") == Some(&json!("fixture-thread")),
                    "read identity"
                );
                emit(
                    json!({"id":id,"result":{"thread":{"id":"fixture-thread","turns":[retained_turn.clone()]}}}),
                )?;
            }
            "turn/interrupt" if controls => {
                std::fs::OpenOptions::new()
                    .create_new(true)
                    .write(true)
                    .open(format!("{marker}.interrupt"))?;
                ensure!(
                    frame.pointer("/params/threadId") == Some(&json!("fixture-thread"))
                        && frame.pointer("/params/turnId") == Some(&json!("fixture-turn")),
                    "interrupt identity"
                );
                emit(json!({"id":id,"result":{}}))?;
                retained_turn["status"] = json!("interrupted");
                retained_turn["items"].as_array_mut().context("items")?.push(json!({"type":"agentMessage","id":"fixture-answer","text":"native fixture response"}));
                std::fs::write(
                    format!("{marker}.thread.json"),
                    serde_json::to_vec(&retained_turn)?,
                )?;
                emit(
                    json!({"method":"turn/completed","params":{"threadId":"fixture-thread","turn":retained_turn.clone()}}),
                )?;
            }
            other => anyhow::bail!("unexpected fixture request: {other}"),
        }
        if exit_after_turn && frame.get("method").and_then(Value::as_str) == Some("turn/start") {
            return Ok(());
        }
    }
    Ok(())
}
