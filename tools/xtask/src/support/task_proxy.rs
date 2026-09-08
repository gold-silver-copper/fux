//! Task receipt failure injection on a fixture-owned control socket.
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
    faults: Mutex<Faults>,
    accepted: AtomicBool,
    release: AtomicBool,
    errors: Mutex<Vec<String>>,
    orphaned: Mutex<Vec<Value>>,
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
#[derive(Clone)]
pub struct Faults {
    pub drop_reserve: bool,
    pub drop_before_submit: bool,
    pub drop_submit: bool,
    pub expire_status: bool,
    pub delay_list: Duration,
    pub delay_status: Duration,
    pub drop_delayed: bool,
    pub hold_submit: bool,
    pub hold_list: bool,
    pub hold_receipt: bool,
}
impl Default for Faults {
    fn default() -> Self {
        Self {
            drop_reserve: true,
            drop_before_submit: true,
            drop_submit: true,
            expire_status: false,
            delay_list: Duration::ZERO,
            delay_status: Duration::ZERO,
            drop_delayed: false,
            hold_submit: false,
            hold_list: false,
            hold_receipt: false,
        }
    }
}
fn pause(state: &State, delay: Duration) -> Result<()> {
    let end = Instant::now() + delay;
    loop {
        if state.stop.load(Ordering::Acquire) {
            return Err(Cancelled.into());
        }
        let left = end.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Ok(());
        }
        thread::sleep(left.min(Duration::from_millis(10)));
    }
}
fn hold(state: &State) -> Result<()> {
    state.accepted.store(true, Ordering::Release);
    let end = Instant::now() + Duration::from_secs(5);
    while !state.release.load(Ordering::Acquire) {
        ensure!(Instant::now() < end, "fixture did not release request");
        pause(state, Duration::from_millis(10))?;
    }
    Ok(())
}
fn handle(mut peer: UnixStream, control: &Path, state: &State) -> Result<()> {
    peer.set_nonblocking(true)?;
    ensure!(line(&mut peer, 8, state)? == b"FUXCTL3\n", "proxy preface");
    write(&mut peer, b"FUXCTL3\n", state)?;
    let request: Value = serde_json::from_slice(&line(&mut peer, 1048576, state)?)?;
    let f = state.faults.lock().unwrap().clone();
    // Freeze an admitted waiter at its next observation request so killing it
    // cannot race a fixture reply write. No request reaches fux in this mode.
    if f.hold_list && request["command"] == "list" {
        hold(state)?;
        return Ok(());
    }
    let delay = match request["command"].as_str() {
        Some("list") => f.delay_list,
        Some("input-status") => f.delay_status,
        _ => Duration::ZERO,
    };
    let discard_delayed = f.drop_delayed && !delay.is_zero();
    pause(state, delay)?;
    {
        let mut f = state.faults.lock().unwrap();
        if request["command"] == "input-submit" && f.drop_before_submit {
            f.drop_before_submit = false;
            return Ok(());
        }
    }
    let mut response =
        if request["command"] == "input-status" && state.faults.lock().unwrap().expire_status {
            json!({"id":1,"status":"failed","error":{"code":"expired"}})
        } else {
            super::local::rpc(control, request.clone())?
        };
    {
        let mut f = state.faults.lock().unwrap();
        if request["command"] == "input-reserve" && f.drop_reserve {
            f.drop_reserve = false;
            state.orphaned.lock().unwrap().push(
                response
                    .pointer("/result/value/receipt/operation")
                    .context("orphan receipt")?
                    .clone(),
            );
            return Ok(());
        }
    }
    if discard_delayed {
        return Ok(());
    }
    let f = state.faults.lock().unwrap().clone();
    if f.hold_receipt
        && matches!(
            request["command"].as_str(),
            Some("input-submit" | "input-status")
        )
        && response["status"] == "completed"
    {
        let receipt = response
            .pointer_mut("/result/value/receipt")
            .context("receipt")?;
        receipt["state"] = json!("queued");
        receipt["bytes_written"] = json!(0);
    }
    if request["command"] == "input-submit" {
        if f.hold_submit {
            hold(state)?;
            return Ok(());
        }
        let mut f = state.faults.lock().unwrap();
        if f.drop_submit {
            f.drop_submit = false;
            return Ok(());
        }
    }
    let mut bytes = serde_json::to_vec(&response)?;
    bytes.push(b'\n');
    write(&mut peer, &bytes, state)
}
pub struct Proxy {
    state: Arc<State>,
    thread: Option<JoinHandle<()>>,
}
impl Proxy {
    pub fn start(directory: &Path, control: &Path) -> Result<Self> {
        let listener = UnixListener::bind(directory.join("default.sock"))?;
        listener.set_nonblocking(true)?;
        let state = Arc::new(State::default());
        let shared = state.clone();
        let control = control.to_owned();
        let thread = thread::spawn(move || {
            let state = shared;
            let result = (|| -> Result<()> {
                while !state.stop.load(Ordering::Acquire) {
                    match listener.accept() {
                        Ok((peer, _)) => handle(peer, &control, &state)?,
                        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                            pause(&state, Duration::from_millis(10))?
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
            state,
            thread: Some(thread),
        })
    }
    pub fn update(&self, change: impl FnOnce(&mut Faults)) {
        change(&mut self.state.faults.lock().unwrap());
    }
    pub fn orphaned(&self) -> Vec<Value> {
        self.state.orphaned.lock().unwrap().clone()
    }
    pub fn reset_hold(&self) {
        self.state.accepted.store(false, Ordering::Release);
        self.state.release.store(false, Ordering::Release);
    }
    pub fn accepted(&self) -> bool {
        self.state.accepted.load(Ordering::Acquire)
    }
    pub fn release(&self) {
        self.state.release.store(true, Ordering::Release);
    }
    pub fn close(&mut self) -> Result<()> {
        self.release();
        self.state.stop.store(true, Ordering::Release);
        if let Some(t) = self.thread.take() {
            let end = Instant::now() + Duration::from_secs(5);
            while !t.is_finished() && Instant::now() < end {
                thread::sleep(Duration::from_millis(10));
            }
            ensure!(t.is_finished(), "task proxy did not stop");
            ensure!(t.join().is_ok(), "task proxy panic");
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
            eprintln!("task proxy cleanup: {e:#}");
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn close_cancels_partial_front_handshake() -> Result<()> {
        let root = tempfile::tempdir()?;
        let mut p = Proxy::start(root.path(), &root.path().join("upstream"))?;
        let mut client = UnixStream::connect(root.path().join("default.sock"))?;
        client.write_all(b"FUX")?;
        thread::sleep(Duration::from_millis(30));
        let start = Instant::now();
        p.close()?;
        ensure!(
            start.elapsed() < Duration::from_secs(1),
            "partial handshake prevented cleanup"
        );
        Ok(())
    }
}
