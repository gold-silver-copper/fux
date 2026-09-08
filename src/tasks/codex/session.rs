//! Native thread establishment for the managed owner. No task-success policy.
use super::transport::Client;
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    process::Command,
    sync::{Arc, atomic::AtomicBool},
    time::Instant,
};

pub struct Session {
    pub client: Client,
    pub thread: String,
    /// Optional provider-reported rollout path. Missing paths do not establish
    /// a namespace for recreation.
    pub rollout: Option<PathBuf>,
    /// Notifications/requests received before the thread response. The owner must
    /// process these after registration; establishing a thread does not consume
    /// or acknowledge any native input-required request.
    pub early_events: Vec<Value>,
}

impl Session {
    /// One process, handshake and thread request under one deadline. On failure
    /// the owned client is dropped; this function never retries thread creation.
    /// Resume callers must also retain and verify the original storage namespace
    /// and managed-worker identity before registering the resulting session.
    pub fn open(
        command: Command,
        cwd: &Path,
        resume: Option<&str>,
        deadline: Instant,
    ) -> Result<Self> {
        Self::open_cancellable(
            command,
            cwd,
            resume,
            deadline,
            Arc::new(AtomicBool::new(false)),
        )
    }

    pub fn open_cancellable(
        mut command: Command,
        cwd: &Path,
        resume: Option<&str>,
        deadline: Instant,
        cancelled: Arc<AtomicBool>,
    ) -> Result<Self> {
        ensure!(cwd.is_absolute(), "native cwd must be absolute");
        let cwd = cwd.canonicalize().context("resolve native cwd")?;
        ensure!(cwd.is_dir(), "native cwd must be a directory");
        let cwd_text = cwd.to_str().context("native cwd must be UTF-8")?;
        if let Some(thread) = resume {
            validate_id(thread)?;
        }
        command.current_dir(&cwd);
        let mut client = Client::spawn_cancellable(command, deadline, cancelled)?;
        client.initialize(deadline)?;
        let request = match resume {
            Some(thread) => json!({"id":"zor-thread","method":"thread/resume","params":{
                "threadId":thread,"cwd":cwd_text,"approvalPolicy":"never","excludeTurns":true
            }}),
            None => json!({"id":"zor-thread","method":"thread/start","params":{
                "cwd":cwd_text,"approvalPolicy":"never","ephemeral":false
            }}),
        };
        client.send(&request, deadline)?;
        let mut early_events = Vec::new();
        let mut event_bytes = 0usize;
        let (thread, rollout) = loop {
            let frame = client.next(deadline)?;
            // Server request IDs are independent of our response namespace.
            if frame.get("method").is_some() {
                ensure!(
                    frame.get("method").and_then(Value::as_str).is_some(),
                    "invalid native event method"
                );
                event_bytes += serde_json::to_vec(&frame)?.len();
                ensure!(
                    early_events.len() < 128 && event_bytes <= 2 * 1024 * 1024,
                    "native thread establishment event limit exceeded"
                );
                early_events.push(frame);
                continue;
            }
            ensure!(
                frame.get("id").and_then(Value::as_str) == Some("zor-thread"),
                "native thread response identity mismatch"
            );
            if let Some(error) = frame.get("error") {
                let detail: String = error
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("provider rejected the thread request")
                    .chars()
                    .take(512)
                    .collect();
                anyhow::bail!("native thread request rejected: {detail}");
            }
            let thread = frame
                .pointer("/result/thread")
                .context("native thread missing")?;
            let id = thread
                .get("id")
                .and_then(Value::as_str)
                .context("native thread ID missing")?;
            validate_id(id)?;
            ensure!(
                resume.is_none_or(|expected| expected == id),
                "native resumed thread identity changed"
            );
            ensure!(
                thread.get("cwd").and_then(Value::as_str) == Some(cwd_text),
                "native thread cwd changed"
            );
            ensure!(
                thread.get("ephemeral").and_then(Value::as_bool) == Some(false),
                "native thread is not persistent"
            );
            let rollout = match thread.get("path") {
                None | Some(Value::Null) => None,
                Some(Value::String(path)) => {
                    ensure!(
                        Path::new(path).is_absolute()
                            && path.len() <= 4096
                            && !path.chars().any(char::is_control),
                        "invalid native rollout path"
                    );
                    Some(PathBuf::from(path))
                }
                Some(_) => anyhow::bail!("invalid native rollout path type"),
            };
            break (id.to_owned(), rollout);
        };
        Ok(Self {
            client,
            thread,
            rollout,
            early_events,
        })
    }
}

fn validate_id(id: &str) -> Result<()> {
    ensure!(
        !id.is_empty() && id.len() <= 256 && !id.chars().any(char::is_control),
        "invalid native thread ID"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    #[ignore = "optional installed thread metadata probe; no model turn"]
    fn installed_thread_exposes_local_storage_identity() -> Result<()> {
        let executable =
            std::env::var_os("ZOR_CODEX_PROBE_BIN").context("ZOR_CODEX_PROBE_BIN missing")?;
        let root = tempfile::tempdir()?;
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut command = Command::new(&executable);
        command.args(["app-server", "--stdio", "-c", "mcp_servers={}"]);
        let session = Session::open(command, root.path(), None, deadline)?;
        let result = (|| -> Result<()> {
            let rollout = session
                .rollout
                .as_deref()
                .context("installed provider has not exposed a rollout path")?;
            let storage = super::super::storage::Storage::capture(rollout)?;
            storage.verify_resumed(rollout)?;
            println!(
                "installed thread/start reported a persistent thread and verifiable local storage namespace; no turn sent"
            );
            Ok(())
        })();
        session
            .client
            .shutdown(Instant::now() + Duration::from_secs(1))?;
        result
    }

    fn fixture(reply: &Value, event: &Value) -> Result<Command> {
        let mut command = Command::new("/bin/sh");
        command.args([
            "-c",
            r#"
IFS= read -r line
printf '%s\n' '{"id":"zor-initialize","result":{}}'
IFS= read -r line
IFS= read -r line
printf '{"method":"fixture/request","params":%s}\n' "$line"
printf '%s\n%s\n' "$1" "$2"
sleep 30 & wait
"#,
            "fixture",
        ]);
        command
            .arg(serde_json::to_string(event)?)
            .arg(serde_json::to_string(reply)?);
        Ok(command)
    }

    #[test]
    fn native_thread_establishment_preserves_early_server_requests() -> Result<()> {
        let root = tempfile::tempdir()?;
        let cwd = root.path().canonicalize()?;
        let reply = json!({"id":"zor-thread","result":{"thread":{
            "id":"thread-a","cwd":cwd,"ephemeral":false
        }}});
        // The server may reuse our response ID in its independent namespace.
        let event = json!({"id":"zor-thread","method":"item/tool/requestUserInput","params":{}});
        for resume in [None, Some("thread-a")] {
            let deadline = Instant::now() + Duration::from_secs(5);
            let session = Session::open(fixture(&reply, &event)?, &cwd, resume, deadline)?;
            assert_eq!(session.thread, "thread-a");
            let request = session.early_events.first().context("observed request")?;
            assert_eq!(request.pointer("/params/params/cwd"), Some(&json!(cwd)));
            assert_eq!(
                request.pointer("/params/params/approvalPolicy"),
                Some(&json!("never"))
            );
            assert_eq!(
                request.pointer("/params/method"),
                Some(&json!(if resume.is_some() {
                    "thread/resume"
                } else {
                    "thread/start"
                }))
            );
            if resume.is_some() {
                assert_eq!(
                    request.pointer("/params/params/threadId"),
                    Some(&json!("thread-a"))
                );
                assert_eq!(
                    request.pointer("/params/params/excludeTurns"),
                    Some(&json!(true))
                );
            } else {
                assert_eq!(
                    request.pointer("/params/params/ephemeral"),
                    Some(&json!(false))
                );
            }
            assert_eq!(session.early_events.last(), Some(&event));
            assert_eq!(session.early_events.len(), 2);
            session.client.shutdown(deadline)?;
        }
        Ok(())
    }

    #[test]
    fn changed_native_identity_directory_or_persistence_rejects_registration() -> Result<()> {
        let root = tempfile::tempdir()?;
        let cwd = root.path().canonicalize()?;
        let base = json!({"id":"zor-thread","result":{"thread":{
            "id":"thread-a","cwd":cwd,"ephemeral":false
        }}});
        for (pointer, value) in [
            ("/id", json!("stale")),
            ("/result/thread/id", json!("thread-b")),
            ("/result/thread/cwd", json!("/changed")),
            ("/result/thread/ephemeral", json!(true)),
        ] {
            let mut reply = base.clone();
            *reply.pointer_mut(pointer).context("fixture field")? = value;
            assert!(
                Session::open(
                    fixture(&reply, &json!({"method":"thread/started","params":{}}))?,
                    &cwd,
                    Some("thread-a"),
                    Instant::now() + Duration::from_secs(5),
                )
                .is_err()
            );
        }
        Ok(())
    }
}
