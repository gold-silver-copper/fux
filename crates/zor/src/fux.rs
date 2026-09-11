//! Authenticated, bounded local fux RPC. Agent policy stays in the caller.
use anyhow::Context;
use serde_json::Value;
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const MAX_REPLY: usize = 1024 * 1024;

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
    stream.set_write_timeout(Some(remaining(limit)?.min(Duration::from_secs(2))))?;
    serde_json::to_writer(&mut stream, &value)?;
    stream.write_all(b"\n")?;
    let deadline = limit.min(Instant::now() + Duration::from_secs(2));
    // A peer may close after buffering a multi-read response. On some Unix systems,
    // changing socket timeouts after closure fails even while unread bytes remain.
    // Poll the fixed deadline and drain nonblocking reads instead.
    use std::os::fd::AsFd;
    stream.set_nonblocking(true)?;
    let mut output = Vec::new();
    loop {
        let remaining = remaining(deadline)?;
        let timeout = u16::try_from(remaining.as_millis().clamp(1, 2000))?;
        let mut polls = [nix::poll::PollFd::new(
            stream.as_fd(),
            nix::poll::PollFlags::POLLIN,
        )];
        match nix::poll::poll(&mut polls, timeout) {
            Ok(0) | Err(nix::errno::Errno::EINTR) => continue,
            Ok(_) => {}
            Err(error) => return Err(error.into()),
        }
        let mut chunk = [0; 8192];
        let n = match stream.read(&mut chunk) {
            Ok(n) => n,
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                ) =>
            {
                continue;
            }
            Err(error) => return Err(error).context("read control response"),
        };
        anyhow::ensure!(n != 0, "control socket closed before a response");
        anyhow::ensure!(
            output.len() + n <= MAX_REPLY,
            "control response exceeds limit"
        );
        output.extend_from_slice(
            chunk
                .get(..n)
                .ok_or_else(|| anyhow::anyhow!("invalid socket read length"))?,
        );
        if output.contains(&b'\n') {
            break;
        }
    }
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
    let timeout = u16::try_from(remaining(limit)?.as_millis().clamp(1, 2000))?;
    use std::os::fd::AsFd;
    let connecting = local_ipc::Connecting::start(path)?;
    if connecting.pending() {
        let mut polls = [nix::poll::PollFd::new(
            connecting.as_fd(),
            nix::poll::PollFlags::POLLOUT,
        )];
        anyhow::ensure!(
            nix::poll::poll(&mut polls, timeout)? > 0,
            "fux connect timed out"
        );
        connecting
            .confirm()
            .map_err(|error| anyhow::anyhow!("fux connect failed: {error}"))?;
    }
    Ok(connecting.finish()?)
}

pub fn completed(socket: &Path, request: Value) -> anyhow::Result<Value> {
    let response = self::request(socket, request)?;
    anyhow::ensure!(
        response.get("status").and_then(Value::as_str) == Some("completed"),
        "control request failed: {response}"
    );
    Ok(response)
}

pub fn runtime() -> anyhow::Result<PathBuf> {
    if let Some(root) = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
    {
        return Ok(root.join("fux"));
    }
    #[cfg(target_os = "macos")]
    if let Some(home) = std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
    {
        return Ok(home.join("Library/Caches/fux-runtime/fux"));
    }
    anyhow::bail!("set XDG_RUNTIME_DIR to the directory used by fux")
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
}
