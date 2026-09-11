//! Authenticated, bounded local fux RPC. Agent policy stays in the caller.
use anyhow::Context;
use serde_json::Value;
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const MAX_REPLY: usize = 1024 * 1024;

/// fux's retention ceilings, as `info` publishes them in `limits.input_retention_ms` and
/// `limits.final_retention_ms`. fux clamps `input-reserve.retain_ms` and `split.final_retain_ms`
/// to these; zor clamps its own policy first so a receipt's `expires_ms` and a record's lifetime
/// are exactly what zor asked for. The test below pins both to fux's `info` reply fixture.
pub const MAX_INPUT_RETENTION_MS: u64 = 600_000;
pub const MAX_FINAL_RETENTION_MS: u64 = 14_400_000;

/// One manager `final` reply, classified. `Pending` (the pane is still live) is the only
/// outcome a caller may poll on; every failure is final for this server instance.
#[derive(Debug)]
pub enum FinalReply<'a> {
    Record(&'a Value),
    Pending,
}

/// Classify a manager `final` envelope. fux answers `pending` (live pane), `conflict` (another
/// server instance), `evicted` (the record cap dropped it under load before its retention
/// elapsed), `expired` (retention elapsed) or `unknown` (no record was ever made, or the id has
/// fallen off fux's bounded rings). `evicted` is reported in its own words because it is a
/// server-side loss, not a policy zor could have chosen better; the others are unavailable
/// evidence and are all hard failures.
pub fn final_reply(response: &Value) -> anyhow::Result<FinalReply<'_>> {
    anyhow::ensure!(
        response.get("reply").and_then(Value::as_str) == Some("final"),
        "unexpected final evidence reply: {response}"
    );
    let result = response
        .get("result")
        .context("final reply without a result")?;
    match result.get("status").and_then(Value::as_str) {
        Some("completed") => result
            .pointer("/result/value/record")
            .map(FinalReply::Record)
            .context("final record missing"),
        Some("failed") => {
            let code = result
                .pointer("/error/code")
                .and_then(Value::as_str)
                .unwrap_or("");
            let message = result
                .pointer("/error/message")
                .and_then(Value::as_str)
                .unwrap_or("");
            match code {
                "pending" => Ok(FinalReply::Pending),
                "evicted" => anyhow::bail!(
                    "fux dropped the final record under load before its retention elapsed; \
                     the exit evidence is lost and cannot be retried ({message})"
                ),
                _ => anyhow::bail!("final evidence unavailable ({code}): {message}"),
            }
        }
        _ => anyhow::bail!("unexpected final evidence reply: {response}"),
    }
}

pub fn request(socket: &Path, value: Value) -> anyhow::Result<Value> {
    request_until(socket, value, Instant::now() + Duration::from_secs(6))
}

/// Open a same-user connection and negotiate the generic control protocol.
pub(crate) fn control(socket: &Path, limit: Instant) -> anyhow::Result<UnixStream> {
    let mut stream = connect(socket, limit).context("connect control socket")?;
    same_user(&stream).context("authenticate control socket")?;
    stream.set_write_timeout(Some(remaining(limit)?.min(Duration::from_secs(2))))?;
    stream.write_all(b"FUX\n")?;
    let deadline = limit.min(Instant::now() + Duration::from_secs(2));
    let mut preface = [0; 4];
    let mut used = 0;
    while used < preface.len() {
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .ok_or_else(|| anyhow::anyhow!("control negotiation timed out"))?;
        stream
            .set_read_timeout(Some(remaining))
            .context("set control read timeout")?;
        let n = stream.read(
            preface
                .get_mut(used..)
                .ok_or_else(|| anyhow::anyhow!("invalid preface offset"))?,
        )?;
        anyhow::ensure!(n != 0, "control server closed during negotiation");
        used += n;
    }
    anyhow::ensure!(&preface == b"FUX\n", "incompatible fux control protocol");
    Ok(stream)
}

pub fn request_until(socket: &Path, value: Value, limit: Instant) -> anyhow::Result<Value> {
    let mut stream = control(socket, limit)?;
    let mut bytes = serde_json::to_vec(&value)?;
    bytes.push(b'\n');
    local_ipc::write_all_until(
        &mut stream,
        &bytes,
        limit.min(Instant::now() + Duration::from_secs(2)),
    )?;
    let deadline = limit.min(Instant::now() + Duration::from_secs(2));
    // A peer may close after buffering a multi-read response. On some Unix systems,
    // changing socket timeouts after closure fails even while unread bytes remain.
    // Poll the fixed deadline and drain nonblocking reads instead.
    stream.set_nonblocking(true)?;
    let output = local_ipc::FrameReader::new(MAX_REPLY)
        .next_frame(&mut stream, deadline)
        .map_err(|error| match error {
            local_ipc::FrameError::TimedOut => anyhow::anyhow!("fux request deadline exceeded"),
            local_ipc::FrameError::Closed => {
                anyhow::anyhow!("control socket closed before a response")
            }
            local_ipc::FrameError::Oversize => anyhow::anyhow!("control response exceeds limit"),
            local_ipc::FrameError::Io(error) => {
                anyhow::Error::from(error).context("read control response")
            }
        })?;
    let response: Value = serde_json::from_slice(&output)?;
    Ok(response)
}

pub(crate) fn same_user(stream: &UnixStream) -> anyhow::Result<()> {
    anyhow::ensure!(
        local_ipc::peer_is_current_user(stream)?,
        "control socket belongs to another user"
    );
    Ok(())
}

fn remaining(limit: Instant) -> anyhow::Result<Duration> {
    limit
        .checked_duration_since(Instant::now())
        .filter(|left| !left.is_zero())
        .ok_or_else(|| anyhow::anyhow!("fux request deadline exceeded"))
}

pub(crate) fn connect(path: &Path, limit: Instant) -> anyhow::Result<UnixStream> {
    local_ipc::connect_until(path, limit).map_err(|error| {
        if error.kind() == std::io::ErrorKind::TimedOut {
            anyhow::anyhow!("fux connect timed out")
        } else {
            error.into()
        }
    })
}

pub fn completed(socket: &Path, request: Value) -> anyhow::Result<Value> {
    let response = self::request(socket, request)?;
    anyhow::ensure!(
        response.get("status").and_then(Value::as_str) == Some("completed"),
        "control request failed: {response}"
    );
    Ok(response)
}

/// fux's runtime directory, discovered exactly as fux discovers it.
pub fn runtime() -> anyhow::Result<PathBuf> {
    local_ipc::runtime_directory("fux")
        .ok_or_else(|| anyhow::anyhow!("set XDG_RUNTIME_DIR to the directory used by fux"))
}

pub fn completed_until(socket: &Path, request: Value, limit: Instant) -> anyhow::Result<Value> {
    let response = request_until(socket, request, limit)?;
    anyhow::ensure!(
        response.get("status").and_then(Value::as_str) == Some("completed"),
        "control request failed: {response}"
    );
    Ok(response)
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn final_replies_are_classified_and_eviction_is_a_hard_failure() {
        let envelope = |result: Value| serde_json::json!({"reply":"final","result":result});
        let record = serde_json::json!({"pane":1,"exit_status":0});
        let completed = envelope(
            serde_json::json!({"id":0,"status":"completed","result":{"kind":"final","value":{"record":record}}}),
        );
        assert!(matches!(
            final_reply(&completed).expect("record"),
            FinalReply::Record(found) if found == &record
        ));
        let failed = |code: &str| {
            envelope(
                serde_json::json!({"id":0,"status":"failed","error":{"code":code,"message":"m"}}),
            )
        };
        assert!(matches!(
            final_reply(&failed("pending")).expect("pending"),
            FinalReply::Pending
        ));
        let evicted = final_reply(&failed("evicted"))
            .expect_err("eviction is a failure")
            .to_string();
        assert!(
            evicted.contains("dropped the final record under load")
                && evicted.contains("cannot be retried"),
            "{evicted}"
        );
        for code in ["expired", "unknown", "conflict"] {
            let error = final_reply(&failed(code))
                .expect_err("unavailable evidence")
                .to_string();
            assert!(
                error.contains("final evidence unavailable") && error.contains(code),
                "{error}"
            );
        }
        assert!(final_reply(&serde_json::json!({"reply":"names","names":[]})).is_err());
    }

    #[test]
    fn caller_deadline_bounds_a_stalled_preface() {
        let path = std::env::temp_dir().join(format!("zor-rpc-{}.sock", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let listener = std::os::unix::net::UnixListener::bind(&path).expect("listener");
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .expect("timeout");
            let mut preface = [0; 4];
            stream.read_exact(&mut preface).expect("preface");
            let mut byte = [0];
            assert_eq!(stream.read(&mut byte).expect("client closes"), 0);
        });
        let started = Instant::now();
        assert!(
            request_until(
                &path,
                serde_json::json!({"command":"list","id":1}),
                started + Duration::from_millis(75)
            )
            .is_err()
        );
        assert!(started.elapsed() < Duration::from_secs(1));
        server.join().expect("join");
        std::fs::remove_file(path).expect("cleanup");
    }

    /// fux's `info` reply fixture (a copy of `crates/fux/tests/verify/fixtures/control/
    /// reply_completed_info.json`, which fux's fixture suite keeps byte-identical).
    const INFO_REPLY: &str = include_str!("../tests/fixtures/control/reply_completed_info.json");

    #[test]
    fn retention_ceilings_match_the_limits_fux_info_publishes() {
        let reply: Value = serde_json::from_str(INFO_REPLY).unwrap_or_default();
        let limits = reply
            .pointer("/result/value/info/limits")
            .cloned()
            .unwrap_or_default();
        assert_eq!(
            limits.get("input_retention_ms").and_then(Value::as_u64),
            Some(MAX_INPUT_RETENTION_MS)
        );
        assert_eq!(
            limits.get("final_retention_ms").and_then(Value::as_u64),
            Some(MAX_FINAL_RETENTION_MS)
        );
    }
}
