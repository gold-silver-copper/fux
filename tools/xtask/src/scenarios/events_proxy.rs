//! Disposable counting/gap/duplicate proxy for the real event-observation scenario.
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
    controls: Mutex<Controls>,
    errors: Mutex<Vec<String>>,
    handlers: Mutex<Vec<JoinHandle<()>>>,
}
#[derive(Default)]
struct Controls {
    generation: u64,
    gap: bool,
    duplicate: bool,
    counts: std::collections::BTreeMap<String, usize>,
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
fn handle(mut peer: UnixStream, upstream: &Path, state: &State) -> Result<()> {
    peer.set_nonblocking(true)?;
    ensure!(line(&mut peer, 8, state)? == b"FUXCTL3\n", "front preface");
    write(&mut peer, b"FUXCTL3\n", state)?;
    let raw = line(&mut peer, 65536, state)?;
    let request: Value = serde_json::from_slice(&raw)?;
    let command = request["command"].as_str().context("command")?;
    let (generation, gap) = {
        let mut c = state.controls.lock().unwrap();
        *c.counts.entry(command.into()).or_default() += 1;
        (c.generation, c.gap)
    };
    if command == "subscribe" && gap {
        let mut bytes = serde_json::to_vec(
            &json!({"id":request.get("id").context("id")?,"status":"failed","error":{"code":"gap","message":"injected replay gap"}}),
        )?;
        bytes.push(b'\n');
        write(&mut peer, &bytes, state)?;
        return Ok(());
    }
    let mut backend =
        crate::support::local::connect(upstream, Instant::now() + Duration::from_secs(3))?;
    backend.set_nonblocking(true)?;
    write(&mut backend, b"FUXCTL3\n", state)?;
    ensure!(
        line(&mut backend, 8, state)? == b"FUXCTL3\n",
        "upstream preface"
    );
    write(&mut backend, &raw, state)?;
    write(&mut peer, &line(&mut backend, 1048576, state)?, state)?;
    if command != "subscribe" {
        return Ok(());
    }
    let mut data = Vec::new();
    while !state.stop.load(Ordering::Acquire) {
        if state.controls.lock().unwrap().generation != generation {
            return Ok(());
        }
        let mut polls = [nix::poll::PollFd::new(
            backend.as_fd(),
            nix::poll::PollFlags::POLLIN,
        )];
        match nix::poll::poll(&mut polls, 100u16) {
            Ok(0) | Err(nix::errno::Errno::EINTR) => continue,
            Ok(_) => {}
            Err(e) => return Err(e.into()),
        }
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
        data.extend_from_slice(&chunk[..n]);
        ensure!(data.len() <= 1048576, "event buffer bound");
        while let Some(end) = data.iter().position(|b| *b == b'\n') {
            let frame = data.drain(..=end).collect::<Vec<_>>();
            let duplicate = std::mem::take(&mut state.controls.lock().unwrap().duplicate);
            write(&mut peer, &frame, state)?;
            if duplicate {
                write(&mut peer, &frame, state)?;
            }
        }
    }
    Ok(())
}
fn failure(state: &State, error: anyhow::Error) {
    // These disconnects are expected during the deliberately broken subscriptions.
    if error.is::<Cancelled>()
        || error.to_string() == "proxy frame EOF"
        || error.downcast_ref::<std::io::Error>().is_some_and(|e| {
            matches!(
                e.kind(),
                std::io::ErrorKind::BrokenPipe | std::io::ErrorKind::ConnectionReset
            )
        })
    {
        return;
    }
    state.errors.lock().unwrap().push(format!("{error:#}"));
}
pub struct Proxy {
    state: Arc<State>,
    acceptor: Option<JoinHandle<()>>,
}
impl Proxy {
    pub fn start(front: &Path, upstream: &Path) -> Result<Self> {
        fs::rename(front, upstream)?;
        let listener = match UnixListener::bind(front) {
            Ok(v) => v,
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
                        let mut handlers = shared.handlers.lock().unwrap();
                        if handlers.len() >= 256
                            || handlers.iter().filter(|h| !h.is_finished()).count() >= 16
                        {
                            failure(&shared, anyhow::anyhow!("fixture connection budget"));
                            break;
                        }
                        let state = shared.clone();
                        let upstream = upstream.clone();
                        handlers.push(thread::spawn(move || {
                            if let Err(e) = handle(peer, &upstream, &state) {
                                failure(&state, e);
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
    pub fn count(&self, command: &str) -> usize {
        self.state
            .controls
            .lock()
            .unwrap()
            .counts
            .get(command)
            .copied()
            .unwrap_or(0)
    }
    pub fn gap(&self, enabled: bool) {
        let mut c = self.state.controls.lock().unwrap();
        c.gap = enabled;
        if enabled {
            c.generation += 1;
        }
    }
    pub fn duplicate(&self) {
        self.state.controls.lock().unwrap().duplicate = true;
    }
    pub fn close(&mut self) -> Result<()> {
        self.state.stop.store(true, Ordering::Release);
        let join = |t: JoinHandle<()>| -> Result<()> {
            let end = Instant::now() + Duration::from_secs(4);
            while !t.is_finished() {
                ensure!(Instant::now() < end, "proxy thread did not stop");
                thread::sleep(Duration::from_millis(10));
            }
            t.join().map_err(|_| anyhow::anyhow!("proxy thread panic"))
        };
        let mut errors = Vec::new();
        if let Some(t) = self.acceptor.take()
            && let Err(e) = join(t)
        {
            errors.push(e.to_string());
        }
        for t in std::mem::take(&mut *self.state.handlers.lock().unwrap()) {
            if let Err(e) = join(t) {
                errors.push(e.to_string());
            }
        }
        errors.extend(self.state.errors.lock().unwrap().iter().cloned());
        ensure!(errors.is_empty(), "{}", errors.join("; "));
        Ok(())
    }
}
impl Drop for Proxy {
    fn drop(&mut self) {
        if self.acceptor.is_some()
            && let Err(e) = self.close()
        {
            eprintln!("event proxy cleanup: {e:#}");
        }
    }
}
