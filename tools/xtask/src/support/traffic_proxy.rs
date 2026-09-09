//! Fixture-only transparent FUX traffic accounting, never linked to production.
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{
    fs,
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
    records: Mutex<Vec<Value>>,
    errors: Mutex<Vec<String>>,
    handlers: Mutex<Vec<JoinHandle<()>>>,
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
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut data = Vec::new();
    loop {
        poll(peer, nix::poll::PollFlags::POLLIN, deadline, state)?;
        let mut byte = [0];
        match peer.read(&mut byte) {
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
fn record(state: &State, value: Value) -> Result<()> {
    state
        .records
        .lock()
        .map_err(|_| anyhow::anyhow!("proxy record mutex"))?
        .push(value);
    Ok(())
}
fn handle(mut peer: UnixStream, upstream: &Path, state: &State, epoch: Instant) -> Result<()> {
    peer.set_nonblocking(true)?;
    ensure!(line(&mut peer, 8, state)? == b"FUX\n", "front preface");
    write(&mut peer, b"FUX\n", state)?;
    let request = line(&mut peer, 65536, state)?;
    let parsed: Value = serde_json::from_slice(&request)?;
    let command = parsed["command"].as_str().context("request command")?;
    let mut backend = super::local::connect(upstream, Instant::now() + Duration::from_secs(3))?;
    backend.set_nonblocking(true)?;
    write(&mut backend, b"FUX\n", state)?;
    ensure!(
        line(&mut backend, 8, state)? == b"FUX\n",
        "upstream preface"
    );
    let started = epoch.elapsed().as_secs_f64();
    write(&mut backend, &request, state)?;
    let reply = line(&mut backend, 1024 * 1024, state)?;
    let value: Value = serde_json::from_slice(&reply)?;
    ensure!(
        value["status"]
            == if command == "subscribe" {
                "accepted"
            } else {
                "completed"
            },
        "unexpected upstream reply: {value}"
    );
    write(&mut peer, &reply, state)?;
    let capture = if command == "capture" {
        value.pointer("/result/value")
    } else {
        None
    };
    let text = capture
        .and_then(|v| v.get("text"))
        .and_then(Value::as_str)
        .unwrap_or("");
    record(
        state,
        json!({"started":started,"finished":epoch.elapsed().as_secs_f64(),"command":command,"request_bytes":request.len(),"reply_bytes":reply.len(),"text_bytes":text.len(),"burst_done":text.contains("BURST_DONE"),"revision":capture.and_then(|v|v.get("revision")).cloned().unwrap_or(Value::Null)}),
    )?;
    if command != "subscribe" {
        return Ok(());
    }
    let mut pending = Vec::new();
    while !state.stop.load(Ordering::Acquire) {
        // Event streams can be idle indefinitely; each individual wait remains cancellable.
        poll(
            &backend,
            nix::poll::PollFlags::POLLIN,
            Instant::now() + Duration::from_secs(24 * 60 * 60),
            state,
        )?;
        let mut chunk = [0; 8192];
        let n = match backend.read(&mut chunk) {
            Ok(0) => return Ok(()),
            Ok(n) => n,
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                ) =>
            {
                continue;
            }
            Err(e) => return Err(e.into()),
        };
        pending.extend_from_slice(&chunk[..n]);
        ensure!(pending.len() <= 1024 * 1024, "event buffer bound");
        while let Some(end) = pending.iter().position(|b| *b == b'\n') {
            let frame = pending.drain(..=end).collect::<Vec<_>>();
            write(&mut peer, &frame, state)?;
            record(
                state,
                json!({"command":"event","finished":epoch.elapsed().as_secs_f64(),"bytes":frame.len()}),
            )?;
        }
    }
    Ok(())
}
fn failure(state: &State, error: anyhow::Error) {
    if error.is::<Cancelled>() {
        return;
    }
    if state.stop.load(Ordering::Acquire)
        && (error.to_string().contains("EOF")
            || error.downcast_ref::<std::io::Error>().is_some_and(|e| {
                matches!(
                    e.kind(),
                    std::io::ErrorKind::BrokenPipe | std::io::ErrorKind::ConnectionReset
                )
            }))
    {
        return;
    }
    state
        .errors
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .push(format!("{error:#}"));
}
pub struct Proxy {
    state: Arc<State>,
    acceptor: Option<JoinHandle<()>>,
}
impl Proxy {
    /// The caller shares one epoch across proxies and observation-window boundaries.
    pub fn start(front: &Path, upstream: &Path, epoch: Instant) -> Result<Self> {
        fs::rename(front, upstream)?;
        let listener = match UnixListener::bind(front) {
            Ok(l) => l,
            Err(e) => {
                fs::rename(upstream, front)?;
                return Err(e.into());
            }
        };
        listener.set_nonblocking(true)?;
        let state = Arc::new(State::default());
        let shared = state.clone();
        let upstream = upstream.to_owned();
        let acceptor = thread::spawn(move || {
            while !shared.stop.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((peer, _)) => {
                        let mut handlers =
                            shared.handlers.lock().unwrap_or_else(|p| p.into_inner());
                        if handlers.len() >= 512 {
                            failure(&shared, anyhow::anyhow!("fixture connection budget"));
                            break;
                        }
                        let state = shared.clone();
                        let upstream = upstream.clone();
                        handlers.push(thread::spawn(move || {
                            if let Err(e) = handle(peer, &upstream, &state, epoch) {
                                failure(&state, e)
                            }
                        }));
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(10))
                    }
                    Err(e) => {
                        failure(&shared, e.into());
                        break;
                    }
                }
            }
        });
        Ok(Self {
            state,
            acceptor: Some(acceptor),
        })
    }
    pub fn snapshot(&self) -> Result<Vec<Value>> {
        Ok(self
            .state
            .records
            .lock()
            .map_err(|_| anyhow::anyhow!("proxy record mutex"))?
            .clone())
    }
    pub fn begin_shutdown(&self) {
        self.state.stop.store(true, Ordering::Release);
    }
    pub fn close(&mut self) -> Result<()> {
        self.state.stop.store(true, Ordering::Release);
        let join = |thread: JoinHandle<()>| -> Result<()> {
            let end = Instant::now() + Duration::from_secs(5);
            while !thread.is_finished() {
                ensure!(Instant::now() < end, "proxy thread did not stop");
                thread::sleep(Duration::from_millis(10));
            }
            thread
                .join()
                .map_err(|_| anyhow::anyhow!("proxy thread panicked"))
        };
        let mut errors = Vec::new();
        if let Some(t) = self.acceptor.take()
            && let Err(e) = join(t)
        {
            errors.push(e.to_string())
        }
        let threads = std::mem::take(
            &mut *self
                .state
                .handlers
                .lock()
                .unwrap_or_else(|p| p.into_inner()),
        );
        for t in threads {
            if let Err(e) = join(t) {
                errors.push(e.to_string())
            }
        }
        errors.extend(
            self.state
                .errors
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .iter()
                .cloned(),
        );
        ensure!(errors.is_empty(), "{}", errors.join("; "));
        Ok(())
    }
}
impl Drop for Proxy {
    fn drop(&mut self) {
        if self.acceptor.is_some()
            && let Err(e) = self.close()
        {
            eprintln!("proxy cleanup: {e:#}")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn forwards_capture_and_subscription_bytes_and_stops_idle_handlers() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let front = temp.path().join("control.sock");
        let upstream = temp.path().join("upstream.sock");
        let listener = UnixListener::bind(&front)?;
        let capture = json!({"id":1,"status":"completed","result":{"value":{"text":"é BURST_DONE","revision":9}}});
        let capture_bytes = [serde_json::to_vec(&capture)?, b"\n".to_vec()].concat();
        let expected_capture = capture_bytes.clone();
        let event = b"{\"event\":{\"sequence\":1}}\n".to_vec();
        let expected_event = event.clone();
        let release = Arc::new(AtomicBool::new(false));
        let backend_release = release.clone();
        let backend = thread::spawn(move || -> Result<()> {
            for command in ["capture", "subscribe"] {
                listener.set_nonblocking(true)?;
                let deadline = Instant::now() + Duration::from_secs(5);
                let mut peer = loop {
                    if backend_release.load(Ordering::Acquire) {
                        return Ok(());
                    }
                    match listener.accept() {
                        Ok((peer, _)) => break peer,
                        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                        Err(e) => return Err(e.into()),
                    }
                    ensure!(Instant::now() < deadline, "fixture backend accept deadline");
                    thread::sleep(Duration::from_millis(10));
                };
                peer.set_nonblocking(true)?;
                let state = State::default();
                ensure!(
                    line(&mut peer, 8, &state)? == b"FUX\n",
                    "preface forwarding"
                );
                write(&mut peer, b"FUX\n", &state)?;
                let request = line(&mut peer, 65536, &state)?;
                let r: Value = serde_json::from_slice(&request)?;
                ensure!(r["command"] == command, "request forwarding");
                if command == "capture" {
                    for fragment in capture_bytes.chunks(3) {
                        write(&mut peer, fragment, &state)?;
                    }
                } else {
                    write(&mut peer, b"{\"id\":2,\"status\":\"accepted\"}\n", &state)?;
                    for fragment in event.chunks(2) {
                        write(&mut peer, fragment, &state)?;
                    }
                    let end = Instant::now() + Duration::from_secs(5);
                    while !backend_release.load(Ordering::Acquire) && Instant::now() < end {
                        thread::sleep(Duration::from_millis(10));
                    }
                }
            }
            Ok(())
        });
        let mut proxy = Proxy::start(&front, &upstream, Instant::now())?;
        let outcome = (|| -> Result<()> {
            let value = super::super::local::rpc(&front, json!({"id":1,"command":"capture"}))?;
            ensure!(value == capture, "capture transparency");
            let mut client = UnixStream::connect(&front)?;
            client.set_nonblocking(true)?;
            let state = State::default();
            write(&mut client, b"FUX\n", &state)?;
            ensure!(line(&mut client, 8, &state)? == b"FUX\n", "client preface");
            let request = b"{\"id\":2,\"command\":\"subscribe\"}\n";
            write(&mut client, request, &state)?;
            ensure!(
                line(&mut client, 1048576, &state)? == b"{\"id\":2,\"status\":\"accepted\"}\n",
                "subscription reply"
            );
            ensure!(
                line(&mut client, 1048576, &state)? == expected_event,
                "event fragmentation transparency"
            );
            super::super::local::until(Duration::from_secs(3), || {
                Ok((proxy.snapshot()?.len() == 3).then_some(()))
            })?;
            let records = proxy.snapshot()?;
            let c = &records[0];
            ensure!(
                c["command"] == "capture"
                    && c["reply_bytes"] == expected_capture.len()
                    && c["text_bytes"] == "é BURST_DONE".len()
                    && c["burst_done"] == true
                    && c["revision"] == 9,
                "capture byte accounting"
            );
            ensure!(
                records[1]["command"] == "subscribe"
                    && records[1]["request_bytes"] == request.len()
                    && records[2]["bytes"] == expected_event.len(),
                "stream accounting"
            );
            proxy.close()?;
            ensure!(
                client.read(&mut [0u8])? == 0,
                "subscription remained open after proxy cleanup"
            );
            Ok(())
        })();
        let cleanup = proxy.close();
        release.store(true, Ordering::Release);
        let backend = backend
            .join()
            .map_err(|_| anyhow::anyhow!("backend panic"))?;
        cleanup?;
        backend?;
        outcome
    }
    #[test]
    fn malformed_preface_is_not_silently_recorded_as_valid_traffic() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let front = temp.path().join("control.sock");
        let upstream = temp.path().join("upstream.sock");
        let _listener = UnixListener::bind(&front)?;
        let mut proxy = Proxy::start(&front, &upstream, Instant::now())?;
        let mut client = UnixStream::connect(&front)?;
        client.write_all(b"INVALID\n")?;
        super::super::local::until(Duration::from_secs(3), || {
            Ok((!proxy.state.errors.lock().unwrap().is_empty()).then_some(()))
        })?;
        ensure!(proxy.snapshot()?.is_empty(), "malformed request counted");
        ensure!(proxy.close().is_err(), "malformed preface ignored");
        Ok(())
    }
}
