//! `zor run`: one command in a throwaway fux workspace, its final screen and exit status.
//!
//! A workflow over fux primitives: the manager's create-only `create`, `split` with an
//! environment and a headless size, the retained `final` record, and workspace `kill`. Nothing
//! is durable and no zor service is involved; the only thing zor owns is the workspace it
//! created, which it releases when the run ends, whatever the outcome.
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// The largest reply frame a run reads; fux's own frame limit.
const MAX_REPLY: usize = 1024 * 1024;

pub struct Run {
    /// Covers workspace creation, the launch and the wait for final evidence.
    pub timeout_ms: u64,
    pub rows: Option<u16>,
    pub columns: Option<u16>,
    pub env: Vec<(String, String)>,
    pub cwd: Option<PathBuf>,
    /// An explicit name; it must not exist yet. The default is unique to this process.
    pub workspace: Option<String>,
    pub argv: Vec<String>,
}

/// The workspace the manager created for this run: its identity pins every later request.
struct Owned {
    name: String,
    stream: u64,
    instance: String,
    control: PathBuf,
}

/// Runs the command and returns the child's exit status as a process exit code.
pub fn run(request: Run) -> Result<u8> {
    if request.argv.is_empty() {
        bail!("run requires a command after `--`");
    }
    let elapsed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|since| since.as_nanos())
        .unwrap_or(0);
    let name = request
        .workspace
        .clone()
        .unwrap_or_else(|| format!("run-{}-{elapsed}", std::process::id()));
    anyhow::ensure!(
        crate::tasks::model::workspace(&name),
        "invalid workspace name {name:?}"
    );
    let cwd = request
        .cwd
        .as_deref()
        .map(std::path::absolute)
        .transpose()?;
    let deadline = Instant::now()
        .checked_add(Duration::from_millis(request.timeout_ms))
        .context("run timeout is too large")?;
    let runtime = crate::fux::runtime()?;
    let owned = create_workspace(&runtime, &name, deadline)?;
    let result = run_in_workspace(&runtime, &owned, &request, cwd, deadline);
    // A reused name or a replacement server must never receive this run's cleanup request.
    let cleanup = release_workspace(&owned);
    match (result, cleanup) {
        (Ok(code), Ok(())) => Ok(u8::try_from(code).unwrap_or(1)),
        (Err(error), Ok(())) => Err(error),
        (Ok(_), Err(error)) => Err(error.context("run workspace cleanup failed")),
        (Err(error), Err(cleanup)) => {
            Err(error.context(format!("run workspace cleanup also failed: {cleanup:#}")))
        }
    }
}

/// Creates the run's workspace on the session server, starting one when none is listening.
/// Creation is create-only in the manager, so a name that already exists is refused rather
/// than borrowed.
fn create_workspace(runtime: &Path, name: &str, deadline: Instant) -> Result<Owned> {
    let manager = runtime.join("manager.sock");
    let control = runtime.join(format!("{name}.sock"));
    let limit = deadline.min(Instant::now() + Duration::from_secs(15));
    let reply = match exchange(&manager, &json!({"request":"create","name":name}), limit) {
        Ok(reply) => reply,
        Err(error) if no_server(&error) => start_server(name, deadline)?,
        Err(error) => return Err(error),
    };
    let descriptor = match reply.get("reply").and_then(Value::as_str) {
        Some("attach") => reply
            .get("descriptor")
            .context("attach descriptor missing")?,
        Some("failed") => bail!(
            "run requires a fresh workspace: {}",
            reply.get("message").and_then(Value::as_str).unwrap_or("")
        ),
        _ => bail!("manager did not confirm workspace creation: {reply}"),
    };
    let stream = descriptor
        .get("stream")
        .and_then(Value::as_u64)
        .filter(|stream| *stream > 0)
        .context("workspace descriptor without a stream")?;
    let instance = descriptor
        .get("instance_nonce")
        .and_then(Value::as_str)
        .filter(|nonce| !nonce.is_empty())
        .context("workspace descriptor without an instance")?
        .to_owned();
    anyhow::ensure!(
        descriptor.get("name").and_then(Value::as_str) == Some(name),
        "manager created a different workspace"
    );
    Ok(Owned {
        name: name.to_owned(),
        stream,
        instance,
        control,
    })
}

/// No session server is listening: `fux workspace new NAME` starts one whose initial workspace
/// is the run's. The descriptor it prints must carry stream 1, the mark of a fresh server's
/// first workspace; anything else means another server won startup and the name was resolved
/// against it, which a run never borrows.
fn start_server(name: &str, deadline: Instant) -> Result<Value> {
    let mut command = std::process::Command::new("fux");
    command.args(["workspace", "new", name]);
    let output = crate::platform::process::run_command(&mut command, deadline)
        .context("start a fux session server (`fux` must be on PATH)")?;
    anyhow::ensure!(
        output.status.success(),
        "fux session server startup failed: {}",
        String::from_utf8_lossy(&output.stderr).trim()
    );
    let reply: Value = serde_json::from_slice(&output.stdout)
        .context("fux workspace new did not print a manager reply")?;
    anyhow::ensure!(
        reply.pointer("/descriptor/stream").and_then(Value::as_u64) == Some(1),
        "initial workspace was replaced during startup"
    );
    Ok(reply)
}

fn run_in_workspace(
    runtime: &Path,
    owned: &Owned,
    request: &Run,
    cwd: Option<PathBuf>,
    deadline: Instant,
) -> Result<u32> {
    let remaining = || {
        deadline
            .checked_duration_since(Instant::now())
            .filter(|duration| !duration.is_zero())
            .with_context(|| {
                format!(
                    "run command did not finish within {} ms",
                    request.timeout_ms
                )
            })
    };
    remaining()?;
    let split = exchange(
        &owned.control,
        &json!({"command":"split","id":2,"instance":owned.instance,"stream":owned.stream,
            "axis":"horizontal","cwd":cwd,"argv":request.argv,"env":request.env,
            "rows":request.rows,"columns":request.columns}),
        deadline,
    )?;
    let pane = match split.get("status").and_then(Value::as_str) {
        Some("completed") => split
            .pointer("/result/value/pane")
            .and_then(Value::as_u64)
            .context("split reply without a pane")?,
        Some("failed") => bail!(
            "run could not start the command: {}",
            split
                .pointer("/error/message")
                .and_then(Value::as_str)
                .unwrap_or("")
        ),
        _ => bail!("unexpected split reply: {split}"),
    };
    // The manager retains authoritative evidence even when the pane exits before its launch
    // reply or its workspace socket disappears; no event connection is needed.
    let manager = runtime.join("manager.sock");
    let record = loop {
        remaining()?;
        let reply = exchange(
            &manager,
            &json!({"request":"final","instance":owned.instance,"pane":pane}),
            deadline,
        )?;
        anyhow::ensure!(
            reply.get("reply").and_then(Value::as_str) == Some("final"),
            "unexpected final evidence reply: {reply}"
        );
        let result = reply
            .get("result")
            .context("final reply without a result")?;
        match result.get("status").and_then(Value::as_str) {
            Some("completed") => {
                break result
                    .pointer("/result/value/record")
                    .cloned()
                    .context("final record missing")?;
            }
            Some("failed") if result.pointer("/error/code") == Some(&json!("pending")) => {
                std::thread::sleep(remaining()?.min(Duration::from_millis(25)));
            }
            Some("failed") => bail!(
                "run final evidence unavailable: {}",
                result
                    .pointer("/error/message")
                    .and_then(Value::as_str)
                    .unwrap_or("")
            ),
            _ => bail!("unexpected final evidence reply: {reply}"),
        }
    };
    anyhow::ensure!(
        record.get("pane").and_then(Value::as_u64) == Some(pane)
            && record.get("workspace").and_then(Value::as_str) == Some(&owned.name)
            && record.get("stream").and_then(Value::as_u64) == Some(owned.stream),
        "run final evidence identity mismatch"
    );
    anyhow::ensure!(
        record.pointer("/capture/truncated") == Some(&json!(false)),
        "run final screen exceeded the retained capture limit"
    );
    let text = record
        .pointer("/capture/text")
        .and_then(Value::as_str)
        .context("final record without capture text")?;
    if !text.is_empty() {
        writeln!(std::io::stdout().lock(), "{text}")?;
    }
    match record.get("exit_status") {
        Some(Value::Number(status)) => status
            .as_u64()
            .and_then(|status| u32::try_from(status).ok())
            .context("invalid final exit status"),
        _ => bail!("run was released before its exit status was observed"),
    }
}

/// Kills the owned workspace, pinned to its instance and stream. A listener that is already gone,
/// a replaced workspace or a missing one all mean there is nothing of ours left to release.
fn release_workspace(owned: &Owned) -> Result<()> {
    let reply = match exchange(
        &owned.control,
        &json!({"command":"workspace","id":3,"instance":owned.instance,"stream":owned.stream,
            "action":{"kill":{"name":owned.name}}}),
        Instant::now() + Duration::from_secs(5),
    ) {
        Ok(reply) => reply,
        Err(error) if no_server(&error) => return Ok(()),
        Err(error) => return Err(error),
    };
    match (
        reply.get("status").and_then(Value::as_str),
        reply.pointer("/error/code").and_then(Value::as_str),
    ) {
        (Some("completed"), _) => Ok(()),
        (Some("failed"), Some("conflict" | "not-found")) => Ok(()),
        _ => bail!("owned workspace was not released: {reply}"),
    }
}

/// One request and its reply on a fresh authenticated connection, every read bounded by
/// `deadline` (the reply to a launch or a final poll may legitimately take longer than the
/// fixed window the generic client uses).
fn exchange(socket: &Path, request: &Value, deadline: Instant) -> Result<Value> {
    let remaining = || {
        deadline
            .checked_duration_since(Instant::now())
            .filter(|left| !left.is_zero())
            .context("fux request deadline exceeded")
    };
    let mut stream = crate::fux::control(socket, deadline)?;
    stream.set_write_timeout(Some(remaining()?.min(Duration::from_secs(2))))?;
    serde_json::to_writer(&mut stream, request)?;
    stream.write_all(b"\n")?;
    stream.set_read_timeout(Some(remaining()?))?;
    let mut reader = BufReader::new(stream.take(MAX_REPLY as u64 + 1));
    let mut line = Vec::new();
    reader
        .read_until(b'\n', &mut line)
        .context("read control response")?;
    anyhow::ensure!(line.len() <= MAX_REPLY, "control response exceeds limit");
    anyhow::ensure!(
        line.pop() == Some(b'\n'),
        "control socket closed before a response"
    );
    serde_json::from_slice(&line).context("decode control response")
}

/// No session server is listening (as opposed to one that answered badly).
fn no_server(error: &anyhow::Error) -> bool {
    error
        .chain()
        .filter_map(|cause| cause.downcast_ref::<std::io::Error>())
        .any(|error| {
            matches!(
                error.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
            )
        })
}

/// `NAME=VALUE` pairs from the command line.
pub fn env_pairs(values: Vec<String>) -> Result<Vec<(String, String)>> {
    values
        .into_iter()
        .map(|pair| {
            let (name, value) = pair
                .split_once('=')
                .with_context(|| format!("--env requires NAME=VALUE, got {pair:?}"))?;
            anyhow::ensure!(!name.is_empty(), "--env requires a non-empty name");
            Ok((name.to_owned(), value.to_owned()))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_pairs_split_on_the_first_equals_and_reject_malformed_entries() {
        let pairs = env_pairs(vec!["A=1".into(), "B=x=y".into(), "C=".into()]).unwrap_or_default();
        assert_eq!(
            pairs,
            vec![
                ("A".to_owned(), "1".to_owned()),
                ("B".to_owned(), "x=y".to_owned()),
                ("C".to_owned(), String::new()),
            ]
        );
        assert!(env_pairs(vec!["NOVALUE".into()]).is_err());
        assert!(env_pairs(vec!["=v".into()]).is_err());
    }

    #[test]
    fn a_missing_socket_is_no_server_and_a_refusal_is_not() {
        let missing = std::env::temp_dir().join(format!("zor-run-missing-{}", std::process::id()));
        let error = exchange(
            &missing,
            &json!({"request":"list"}),
            Instant::now() + Duration::from_secs(1),
        )
        .err()
        .map(|error| no_server(&error));
        assert_eq!(error, Some(true));
        assert!(!no_server(&anyhow::anyhow!(
            "run requires a fresh workspace"
        )));
    }

    #[test]
    fn an_empty_command_and_an_invalid_name_are_rejected_before_any_socket_is_touched() {
        let empty = run(Run {
            timeout_ms: 10,
            rows: None,
            columns: None,
            env: Vec::new(),
            cwd: None,
            workspace: None,
            argv: Vec::new(),
        });
        assert!(empty.is_err());
        let invalid = run(Run {
            timeout_ms: 10,
            rows: None,
            columns: None,
            env: Vec::new(),
            cwd: None,
            workspace: Some("../x".into()),
            argv: vec!["/bin/true".into()],
        });
        assert!(invalid.is_err());
    }
}
