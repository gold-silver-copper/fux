//! Authenticated, bounded local fux RPC. Agent policy stays in the caller.
pub(crate) mod capture;
pub(crate) mod endpoint;
pub(crate) mod error;
pub(crate) mod events;
pub(crate) mod info;
pub(crate) mod input;
pub(crate) mod manager;
pub(crate) mod pane;
pub(crate) mod snapshot;
pub(crate) mod subscription;

use anyhow::Context;
use serde_json::Value;
#[cfg(test)]
use std::io::Read;
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

/// Open a same-user connection and negotiate the generic control protocol.
fn control(socket: &Path, limit: Instant) -> anyhow::Result<UnixStream> {
    let mut stream = connect(socket, limit).context("connect control socket")?;
    same_user(&stream).context("authenticate control socket")?;
    let deadline = limit.min(Instant::now() + Duration::from_secs(2));
    local_ipc::write_all_until(&mut stream, b"FUX\n", deadline)
        .map_err(error::TransportFailure::Io)?;
    let mut preface = [0; 4];
    local_ipc::read_exact_until(&mut stream, &mut preface, deadline)
        .map_err(error::TransportFailure::Io)
        .context("read control preface")?;
    if &preface != b"FUX\n" {
        return Err(error::malformed(anyhow::anyhow!(
            "incompatible fux control protocol"
        )));
    }
    Ok(stream)
}

fn request_until(socket: &Path, value: Value, limit: Instant) -> anyhow::Result<Value> {
    let mut stream = control(socket, limit)?;
    let mut bytes = serde_json::to_vec(&value)?;
    bytes.push(b'\n');
    local_ipc::write_all_until(
        &mut stream,
        &bytes,
        limit.min(Instant::now() + Duration::from_secs(2)),
    )
    .map_err(error::TransportFailure::Io)?;
    let deadline = limit.min(Instant::now() + Duration::from_secs(2));
    // A peer may close after buffering a multi-read response. On some Unix systems,
    // changing socket timeouts after closure fails even while unread bytes remain.
    // Poll the fixed deadline and drain nonblocking reads instead.
    stream
        .set_nonblocking(true)
        .map_err(error::TransportFailure::Io)?;
    let output = local_ipc::FrameReader::new(MAX_REPLY)
        .next_frame(&mut stream, deadline)
        .map_err(|error| match error {
            local_ipc::FrameError::TimedOut => error::TransportFailure::Deadline.into(),
            local_ipc::FrameError::Closed => error::TransportFailure::Closed.into(),
            local_ipc::FrameError::Oversize => {
                error::malformed(anyhow::anyhow!("control response exceeds limit"))
            }
            local_ipc::FrameError::Io(error) => {
                anyhow::Error::from(self::error::TransportFailure::Io(error))
                    .context("read control response")
            }
        })?;
    let response: Value = serde_json::from_slice(&output).map_err(error::malformed)?;
    Ok(response)
}

fn same_user(stream: &UnixStream) -> anyhow::Result<()> {
    if !local_ipc::peer_is_current_user(stream).map_err(error::TransportFailure::Io)? {
        return Err(error::TransportFailure::Io(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "control socket belongs to another user",
        ))
        .into());
    }
    Ok(())
}

fn connect(path: &Path, limit: Instant) -> anyhow::Result<UnixStream> {
    local_ipc::connect_until(path, limit)
        .map_err(|error| self::error::TransportFailure::Io(error).into())
}

/// fux's runtime directory, discovered exactly as fux discovers it.
pub fn runtime() -> anyhow::Result<PathBuf> {
    local_ipc::runtime_directory("fux")
        .ok_or_else(|| anyhow::anyhow!("set XDG_RUNTIME_DIR to the directory used by fux"))
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;

    use std::io::{BufRead, BufReader, Write};

    #[test]
    fn framing_preserves_failure_categories_and_drains_closed_peers() -> anyhow::Result<()> {
        for (bytes, expected) in [
            (b"{invalid}\n".to_vec(), Some(error::Kind::MalformedReply)),
            (vec![b'x'; MAX_REPLY + 1], Some(error::Kind::MalformedReply)),
            (
                b"{\"unfinished\":true}".to_vec(),
                Some(error::Kind::Transport),
            ),
            (
                format!("{{\"text\":\"{}\"}}\n", "x".repeat(65536)).into_bytes(),
                None,
            ),
        ] {
            let root = tempfile::tempdir()?;
            let socket = root.path().join("rpc.sock");
            let listener = std::os::unix::net::UnixListener::bind(&socket)?;
            let peer = std::thread::spawn(move || -> std::io::Result<()> {
                let (mut stream, _) = listener.accept()?;
                stream.set_read_timeout(Some(Duration::from_secs(3)))?;
                stream.set_write_timeout(Some(Duration::from_secs(3)))?;
                let mut preface = [0; 4];
                stream.read_exact(&mut preface)?;
                stream.write_all(&preface)?;
                let mut request = String::new();
                BufReader::new(&mut stream).read_line(&mut request)?;
                // Oversize rejection may close the socket before the producer finishes writing.
                if let Err(error) = stream.write_all(&bytes)
                    && !matches!(
                        error.kind(),
                        std::io::ErrorKind::BrokenPipe | std::io::ErrorKind::ConnectionReset
                    )
                {
                    return Err(error);
                }
                Ok(())
            });
            let result = request_until(
                &socket,
                serde_json::json!({"request":"list"}),
                Instant::now() + Duration::from_secs(3),
            );
            peer.join()
                .map_err(|_| anyhow::anyhow!("peer panicked"))??;
            match expected {
                Some(expected) => {
                    let failure = result.err().context("expected frame rejection")?;
                    assert_eq!(error::kind(&failure), Some(expected));
                    assert_eq!(error::remote_code(&failure), None);
                }
                None => assert_eq!(
                    result?.get("text").and_then(Value::as_str).map(str::len),
                    Some(65536)
                ),
            }
        }
        Ok(())
    }

    #[test]
    fn partial_reply_progress_does_not_renew_the_request_deadline() -> anyhow::Result<()> {
        use std::os::unix::net::UnixListener;
        let root = tempfile::tempdir()?;
        let socket = root.path().join("rpc.sock");
        let listener = UnixListener::bind(&socket)?;
        let peer = std::thread::spawn(move || -> std::io::Result<()> {
            let (mut stream, _) = listener.accept()?;
            stream.set_read_timeout(Some(Duration::from_secs(3)))?;
            let mut preface = [0; 4];
            stream.read_exact(&mut preface)?;
            stream.write_all(&preface)?;
            let mut request = String::new();
            BufReader::new(&mut stream).read_line(&mut request)?;
            // Valid JSON arrives continuously, but finishing it exceeds the total deadline.
            for byte in b"{\"padding\":\"abcdefghijklmnopqrstuvwxyz\"}\n" {
                if stream.write_all(&[*byte]).is_err() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(30));
            }
            Ok(())
        });
        let result = request_until(
            &socket,
            serde_json::json!({"request":"list"}),
            Instant::now() + Duration::from_millis(300),
        );
        peer.join()
            .map_err(|_| anyhow::anyhow!("peer panicked"))??;
        assert!(
            result.is_err(),
            "partial reads renewed the absolute deadline"
        );
        Ok(())
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
