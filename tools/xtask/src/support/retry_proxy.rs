//! Retry comparison faults on fixture-owned fux/herdr control sockets.
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{
    io::{Read, Write},
    os::{
        fd::AsFd,
        unix::net::{UnixListener, UnixStream},
    },
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
#[derive(Debug)]
struct Cancelled;
impl std::fmt::Display for Cancelled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("proxy stopping")
    }
}
impl std::error::Error for Cancelled {}
#[derive(Default)]
struct State {
    stop: AtomicBool,
    errors: Mutex<Vec<String>>,
    events: Mutex<Vec<Value>>,
    dropped: AtomicBool,
}
fn poll(
    peer: &UnixStream,
    flags: nix::poll::PollFlags,
    deadline: Instant,
    state: &State,
) -> Result<()> {
    loop {
        if state.stop.load(Ordering::Acquire) {
            return Err(Cancelled.into());
        }
        let left = deadline
            .checked_duration_since(Instant::now())
            .context("proxy I/O deadline")?;
        let mut fds = [nix::poll::PollFd::new(peer.as_fd(), flags)];
        match nix::poll::poll(&mut fds, u16::try_from(left.as_millis().clamp(1, 100))?) {
            Ok(0) | Err(nix::errno::Errno::EINTR) => continue,
            Ok(_) => return Ok(()),
            Err(e) => return Err(e.into()),
        }
    }
}
fn write(peer: &mut UnixStream, bytes: &[u8], state: &State) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut offset = 0;
    while offset < bytes.len() {
        poll(peer, nix::poll::PollFlags::POLLOUT, deadline, state)?;
        match peer.write(&bytes[offset..]) {
            Ok(0) => anyhow::bail!("proxy write EOF"),
            Ok(n) => offset += n,
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                ) => {}
            Err(e) => return Err(e.into()),
        }
    }
    Ok(())
}
fn line(peer: &mut UnixStream, maximum: usize, state: &State) -> Result<Vec<u8>> {
    line_with_timeout(peer, maximum, state, Duration::from_secs(3))
}
fn line_with_timeout(
    peer: &mut UnixStream,
    maximum: usize,
    state: &State,
    timeout: Duration,
) -> Result<Vec<u8>> {
    let deadline = Instant::now() + timeout;
    let mut data = Vec::new();
    loop {
        poll(peer, nix::poll::PollFlags::POLLIN, deadline, state)?;
        let mut byte = [0];
        match peer.read(&mut byte) {
            Ok(0) if data.is_empty() => return Ok(data),
            Ok(0) => anyhow::bail!("proxy frame EOF"),
            Ok(_) => {
                data.push(byte[0]);
                ensure!(data.len() <= maximum, "proxy frame bound");
                if byte[0] == b'\n' {
                    return Ok(data);
                }
            }
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                ) => {}
            Err(e) => return Err(e.into()),
        }
    }
}
fn handle(
    mut peer: UnixStream,
    upstream: &Path,
    backend: &str,
    fault: &str,
    received: &Path,
    state: &State,
) -> Result<()> {
    use sha2::{Digest, Sha256};
    peer.set_nonblocking(true)?;
    if backend == "zor" {
        ensure!(line(&mut peer, 8, state)? == b"FUXCTL3\n", "proxy preface");
        write(&mut peer, b"FUXCTL3\n", state)?;
    }
    let raw = line(&mut peer, 1048576, state)?;
    let request: Value = serde_json::from_slice(&raw)?;
    let method = request
        .get("method")
        .or_else(|| request.get("command"))
        .context("method")?;
    let submit = method
        == if backend == "herdr" {
            "agent.prompt"
        } else {
            "input-submit"
        };
    let drop = submit && !state.dropped.load(Ordering::Acquire);
    let mut event = json!({"method":method,"id":request.get("id").context("request ID")?,"operation":request.get("operation"),"request_sha256":format!("{:x}",Sha256::digest(&raw))});
    if drop && fault == "before-forward" {
        event["effect"] = json!("request-dropped");
        state.dropped.store(true, Ordering::Release);
        state.events.lock().unwrap().push(event);
        return Ok(());
    }
    let response = if backend == "zor" {
        super::local::rpc(upstream, request)?
    } else {
        super::service::rpc(upstream, &request, Duration::from_secs(10))?
    };
    event["effect"] = json!("forwarded");
    if drop {
        ensure!(
            response.get("error").is_none()
                && response.get("status").is_none_or(|s| s == "completed"),
            "dropped reply failed: {response}"
        );
        super::local::until(Duration::from_secs(8), || {
            if state.stop.load(Ordering::Acquire) {
                return Err(Cancelled.into());
            }
            Ok(
                (received.exists() && std::fs::read_to_string(received)? == "retry-probe\n")
                    .then_some(()),
            )
        })?;
        event["effect"] = json!("reply-dropped-after-consumption");
        state.dropped.store(true, Ordering::Release);
        state.events.lock().unwrap().push(event);
        return Ok(());
    }
    state.events.lock().unwrap().push(event);
    let mut bytes = serde_json::to_vec(&response)?;
    bytes.push(b'\n');
    write(&mut peer, &bytes, state)
}
pub struct Proxy {
    pub path: std::path::PathBuf,
    state: Arc<State>,
    thread: Option<JoinHandle<()>>,
}
impl Proxy {
    pub fn start(
        path: &Path,
        upstream: &Path,
        backend: &str,
        fault: &str,
        received: &Path,
    ) -> Result<Self> {
        ensure!(
            ["zor", "herdr"].contains(&backend)
                && ["before-forward", "after-consumption"].contains(&fault),
            "proxy mode"
        );
        let listener = UnixListener::bind(path)?;
        listener.set_nonblocking(true)?;
        let state = Arc::new(State::default());
        let shared = state.clone();
        let upstream = upstream.to_owned();
        let backend = backend.to_owned();
        let fault = fault.to_owned();
        let received = received.to_owned();
        let thread = thread::spawn(move || {
            let state = shared;
            let result = (|| -> Result<()> {
                while !state.stop.load(Ordering::Acquire) {
                    match listener.accept() {
                        Ok((peer, _)) => {
                            handle(peer, &upstream, &backend, &fault, &received, &state)?
                        }
                        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                            thread::sleep(Duration::from_millis(10))
                        }
                        Err(e) => return Err(e.into()),
                    }
                }
                Ok(())
            })();
            if let Err(e) = result
                && !e.is::<Cancelled>()
            {
                state.errors.lock().unwrap().push(format!("{e:#}"));
            }
        });
        Ok(Self {
            path: path.to_owned(),
            state,
            thread: Some(thread),
        })
    }
    pub fn events(&self) -> Vec<Value> {
        self.state.events.lock().unwrap().clone()
    }
    pub fn dropped(&self) -> bool {
        self.state.dropped.load(Ordering::Acquire)
    }
    pub fn close(&mut self) -> Result<()> {
        self.state.stop.store(true, Ordering::Release);
        if let Some(t) = self.thread.take() {
            let end = Instant::now() + Duration::from_secs(12);
            while !t.is_finished() && Instant::now() < end {
                thread::sleep(Duration::from_millis(10));
            }
            ensure!(t.is_finished(), "proxy did not stop");
            ensure!(t.join().is_ok(), "proxy panic");
        }
        let errors = self.state.errors.lock().unwrap();
        ensure!(errors.is_empty(), "{}", errors.join("; "));
        Ok(())
    }
}
impl Drop for Proxy {
    fn drop(&mut self) {
        if self.thread.is_some()
            && let Err(e) = self.close()
        {
            eprintln!("retry proxy cleanup: {e:#}");
        }
    }
}
/// EOF is the deliberately lost reply; partial or malformed JSON remains failure.
pub fn raw_prompt(path: &Path, request: &Value) -> Result<Value> {
    let mut peer = super::local::connect(path, Instant::now() + Duration::from_secs(10))?;
    peer.set_nonblocking(true)?;
    let state = State::default();
    let mut bytes = serde_json::to_vec(request)?;
    bytes.push(b'\n');
    write(&mut peer, &bytes, &state)?;
    let raw = line_with_timeout(&mut peer, 1048576, &state, Duration::from_secs(10))?;
    if raw.is_empty() {
        Ok(json!({"transport":"eof"}))
    } else {
        Ok(serde_json::from_slice(&raw)?)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn partial_reply_is_not_lost_reply_evidence() -> Result<()> {
        let (mut client, mut server) = UnixStream::pair()?;
        client.set_nonblocking(true)?;
        server.write_all(b"{\"partial\":")?;
        drop(server);
        ensure!(
            line(&mut client, 1048576, &State::default()).is_err(),
            "partial reply accepted as EOF"
        );
        let (mut client, server) = UnixStream::pair()?;
        client.set_nonblocking(true)?;
        drop(server);
        ensure!(
            line(&mut client, 1048576, &State::default())?.is_empty(),
            "clean EOF refused"
        );
        Ok(())
    }
}
