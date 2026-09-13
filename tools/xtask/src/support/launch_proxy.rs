//! Managed-launch fixture faults on control and manager sockets; no production linkage.
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
    timings: Mutex<Option<Vec<RequestTiming>>>,
}
struct RequestTiming {
    operation: &'static str,
    accepted: Instant,
    requested: Instant,
    finished: Instant,
}
struct TimingGuard<'a> {
    state: &'a State,
    operation: &'static str,
    accepted: Instant,
    requested: Instant,
}
impl Drop for TimingGuard<'_> {
    fn drop(&mut self) {
        let finished = Instant::now();
        if let Some(timings) = self.state.timings.lock().unwrap().as_mut()
            && timings.len() < 256
        {
            timings.push(RequestTiming {
                operation: self.operation,
                accepted: self.accepted,
                requested: self.requested,
                finished,
            });
        }
    }
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
#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub enum Mode {
    #[default]
    DropAfter,
    DropBefore,
    Hold,
    WaitExit,
    Normal,
}
#[derive(Clone, Default)]
pub struct Faults {
    pub mode: Mode,
    pub creates: usize,
    pub kills: usize,
    pub observation_failures: usize,
    pub bad_final: bool,
    pub gap: bool,
    pub expired: bool,
    pub duplicate: bool,
    pub vanish: bool,
    pub large_final: bool,
    pub unavailable: bool,
    pub drop_kill: bool,
    pub hold_kill: bool,
    pub drop_pin_before: bool,
    pub drop_pin_reply: bool,
    pub exit_before_pin: bool,
}
fn checkpoint(state: &State) -> Result<()> {
    if state.stop.load(Ordering::Acquire) {
        Err(Cancelled.into())
    } else {
        Ok(())
    }
}
fn panes(control: &Path) -> Result<Vec<Value>> {
    let listing = super::local::completed(control, json!({"id":1,"command":"list"}))?;
    let mut panes = Vec::new();
    for w in listing["workspaces"].as_array().context("workspaces")? {
        for t in w["tabs"].as_array().context("tabs")? {
            panes.extend(t["panes"].as_array().context("panes")?.iter().cloned());
        }
    }
    Ok(panes)
}
fn hold(state: &State) -> Result<()> {
    state.accepted.store(true, Ordering::Release);
    let end = Instant::now() + Duration::from_secs(10);
    while !state.release.load(Ordering::Acquire) {
        checkpoint(state)?;
        ensure!(Instant::now() < end, "fixture did not release held reply");
        thread::sleep(Duration::from_millis(10));
    }
    Ok(())
}
fn alter(response: &mut Value, request: &Value, faults: &Faults) -> Result<()> {
    if request["command"] == "events" && faults.duplicate {
        let events = response
            .pointer_mut("/result/value/events")
            .and_then(Value::as_array_mut)
            .context("events")?;
        let copies = events
            .iter()
            .filter(|e| e["event"] == "pane.opened")
            .cloned()
            .collect::<Vec<_>>();
        events.extend(copies);
    }
    if request["request"] == "final" && faults.expired {
        *response =
            json!({"reply":"final","result":{"id":0,"status":"failed","error":{"code":"expired"}}});
    }
    if request["command"] == "events" && faults.gap {
        *response = json!({"id":1,"status":"failed","error":{"code":"gap"}});
    }
    if request["request"] == "final"
        && response.pointer("/result/status") == Some(&json!("completed"))
    {
        if faults.large_final {
            let capture = response
                .pointer_mut("/result/result/value/record/capture")
                .context("final capture")?;
            capture["text"] = json!(format!("x{}", "🐈".repeat(2048)));
            capture["truncated"] = json!(false);
        }
        if faults.bad_final {
            let stream = response
                .pointer_mut("/result/result/value/record/stream")
                .context("stream")?;
            *stream = json!(stream.as_u64().context("stream number")? + 1);
        }
    }
    Ok(())
}
fn handle(
    mut peer: UnixStream,
    destination: &Path,
    control: &Path,
    manager: &Path,
    instance: &Value,
    state: &State,
) -> Result<()> {
    let accepted = Instant::now();
    peer.set_nonblocking(true)?;
    ensure!(line(&mut peer, 4, state)? == b"FUX\n", "proxy preface");
    write(&mut peer, b"FUX\n", state)?;
    let request: Value = serde_json::from_slice(&line(&mut peer, 1048576, state)?)?;
    let _timing = state
        .timings
        .lock()
        .unwrap()
        .is_some()
        .then(|| TimingGuard {
            state,
            operation: match request["request"].as_str().or(request["command"].as_str()) {
                Some("pane-location") => "pane-location",
                Some("release-pane-pin") => "release-pane-pin",
                Some("list") => "list",
                Some("final") => "final",
                _ => "other",
            },
            accepted,
            requested: Instant::now(),
        });
    {
        let mut f = state.faults.lock().unwrap();
        if request["command"] == "kill" {
            f.kills += 1;
        }
        if request["command"] == "split" {
            f.creates += 1;
            if f.mode == Mode::DropBefore {
                return Ok(());
            }
        }
        if (request["command"] == "list" || request["request"] == "pane-location")
            && (f.unavailable || f.observation_failures > 0)
        {
            f.observation_failures = f.observation_failures.saturating_sub(1);
            drop(f);
            let response = if request["request"] == "pane-location" {
                json!({"reply":"pane-location","result":{"id":0,"status":"failed","error":{"code":"internal"}}})
            } else {
                json!({"id":1,"status":"failed","error":{"code":"internal"}})
            };
            let mut bytes = serde_json::to_vec(&response)?;
            bytes.push(b'\n');
            write(&mut peer, &bytes, state)?;
            return Ok(());
        }
    }
    if request["request"] == "release-pane-pin" {
        let mut faults = state.faults.lock().unwrap();
        if faults.drop_pin_before {
            faults.drop_pin_before = false;
            return Ok(());
        }
        let exit_before_pin = std::mem::take(&mut faults.exit_before_pin);
        drop(faults);
        if exit_before_pin {
            let pane = request.get("pane").context("release pane")?;
            super::local::completed(
                control,
                json!({"command":"kill","id":1,"instance":instance,"pane":pane}),
            )?;
            super::local::until(Duration::from_secs(3), || {
                checkpoint(state)?;
                let reply = super::local::rpc(
                    manager,
                    json!({"request":"final","instance":instance,"pane":pane}),
                )?;
                Ok((reply["result"]["status"] == "completed").then_some(()))
            })?;
        }
    }
    if !destination.exists() {
        return Ok(());
    }
    let mut response = super::local::rpc(destination, request.clone())?;
    if request["request"] == "release-pane-pin" {
        let mut faults = state.faults.lock().unwrap();
        if faults.drop_pin_reply {
            faults.drop_pin_reply = false;
            return Ok(());
        }
    }
    let faults = state.faults.lock().unwrap().clone();
    if request["command"] == "kill" {
        if faults.hold_kill {
            hold(state)?;
            return Ok(());
        }
        if faults.drop_kill {
            return Ok(());
        }
    }
    alter(&mut response, &request, &faults)?;
    if request["command"] == "split" {
        match faults.mode {
            Mode::WaitExit => {
                let pane = response
                    .pointer("/result/value/pane")
                    .context("created pane")?;
                super::local::until(Duration::from_secs(3), || {
                    checkpoint(state)?;
                    Ok(panes(control)?
                        .iter()
                        .all(|p| p.get("id") != Some(pane))
                        .then_some(()))
                })?;
                super::local::until(Duration::from_secs(3), || {
                    checkpoint(state)?;
                    let v = super::local::rpc(
                        manager,
                        json!({"request":"final","instance":instance,"pane":pane}),
                    )?;
                    Ok((v["result"]["status"] == "completed").then_some(()))
                })?;
                if faults.vanish {
                    super::local::rpc(manager, json!({"request":"kill","name":"default"}))?;
                    super::local::until(Duration::from_secs(3), || {
                        checkpoint(state)?;
                        Ok((!control.exists()).then_some(()))
                    })?;
                }
            }
            Mode::Hold => {
                hold(state)?;
                return Ok(());
            }
            Mode::DropAfter => return Ok(()),
            _ => {}
        }
    }
    let mut bytes = serde_json::to_vec(&response)?;
    bytes.push(b'\n');
    write(&mut peer, &bytes, state)
}
pub struct Proxy {
    state: Arc<State>,
    threads: Vec<JoinHandle<()>>,
}
impl Proxy {
    pub fn start(
        directory: &Path,
        control: &Path,
        manager: &Path,
        instance: Value,
    ) -> Result<Self> {
        let mut listeners = Vec::new();
        for name in ["default.sock", "manager.sock"] {
            let listener = UnixListener::bind(directory.join(name))?;
            listener.set_nonblocking(true)?;
            listeners.push(listener);
        }
        let state = Arc::new(State::default());
        let mut threads = Vec::new();
        for (listener, destination) in listeners.into_iter().zip([control, manager]) {
            let state = state.clone();
            let control = control.to_owned();
            let manager = manager.to_owned();
            let destination = destination.to_owned();
            let instance = instance.clone();
            threads.push(thread::spawn(move || {
                let result = (|| -> Result<()> {
                    while !state.stop.load(Ordering::Acquire) {
                        match listener.accept() {
                            Ok((peer, _)) => {
                                handle(peer, &destination, &control, &manager, &instance, &state)?
                            }
                            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                                // Wake as soon as a queued connection is readable. An
                                // unconditional sleep adds fixture latency to every RPC.
                                // Keep the same maximum cancellation-check interval.
                                let mut fds = [nix::poll::PollFd::new(
                                    listener.as_fd(),
                                    nix::poll::PollFlags::POLLIN,
                                )];
                                match nix::poll::poll(&mut fds, 10u16) {
                                    Ok(_) | Err(nix::errno::Errno::EINTR) => {}
                                    Err(error) => return Err(error.into()),
                                }
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
            }));
        }
        Ok(Self { state, threads })
    }
    pub fn update(&self, change: impl FnOnce(&mut Faults)) {
        change(&mut self.state.faults.lock().unwrap());
    }
    pub fn faults(&self) -> Faults {
        self.state.faults.lock().unwrap().clone()
    }
    /// Opt-in, bounded timings for synthetic benchmark requests; never retain payloads.
    pub fn begin_timing(&self) {
        *self.state.timings.lock().unwrap() = Some(Vec::new());
    }
    pub fn finish_timing(&self, epoch: Instant) -> Value {
        let timings = self
            .state
            .timings
            .lock()
            .unwrap()
            .take()
            .unwrap_or_default();
        json!(
            timings
                .into_iter()
                .map(|timing| {
                    let ms =
                        |at: Instant| at.saturating_duration_since(epoch).as_secs_f64() * 1000.;
                    json!({"operation":timing.operation,"accepted_ms":ms(timing.accepted),
                "requested_ms":ms(timing.requested),"finished_ms":ms(timing.finished)})
                })
                .collect::<Vec<_>>()
        )
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
        let mut errors = Vec::new();
        for t in self.threads.drain(..) {
            let end = Instant::now() + Duration::from_secs(5);
            while !t.is_finished() && Instant::now() < end {
                thread::sleep(Duration::from_millis(10));
            }
            if !t.is_finished() {
                errors.push("launch proxy did not stop".into());
            } else if t.join().is_err() {
                errors.push("launch proxy panic".into());
            }
        }
        errors.extend(self.state.errors.lock().unwrap().iter().cloned());
        ensure!(errors.is_empty(), "{}", errors.join("; "));
        Ok(())
    }
}
impl Drop for Proxy {
    fn drop(&mut self) {
        if !self.threads.is_empty()
            && let Err(e) = self.close()
        {
            eprintln!("launch proxy cleanup: {e:#}");
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn final_and_event_faults_preserve_original_injections() -> Result<()> {
        let request = json!({"request":"final"});
        let original = json!({"result":{"status":"completed","result":{"value":{"record":{"stream":7,"capture":{"text":"ok","truncated":true}}}}}});
        let mut altered = original.clone();
        alter(
            &mut altered,
            &request,
            &Faults {
                large_final: true,
                bad_final: true,
                ..Faults::default()
            },
        )?;
        ensure!(
            altered.pointer("/result/result/value/record/stream") == Some(&json!(8)),
            "stream mismatch fault"
        );
        ensure!(
            altered.pointer("/result/result/value/record/capture/text")
                == Some(&json!(format!("x{}", "🐈".repeat(2048))))
                && altered.pointer("/result/result/value/record/capture/truncated")
                    == Some(&json!(false)),
            "UTF-8 final fault"
        );
        let mut expired = original;
        alter(
            &mut expired,
            &request,
            &Faults {
                expired: true,
                large_final: true,
                bad_final: true,
                ..Faults::default()
            },
        )?;
        ensure!(
            expired
                == json!({"reply":"final","result":{"id":0,"status":"failed","error":{"code":"expired"}}}),
            "expiry precedence"
        );
        let mut events = json!({"result":{"value":{"events":[{"event":"pane.opened","id":1},{"event":"pane.closed","id":2}]}}});
        alter(
            &mut events,
            &json!({"command":"events"}),
            &Faults {
                duplicate: true,
                ..Faults::default()
            },
        )?;
        ensure!(
            events["result"]["value"]["events"]
                .as_array()
                .unwrap()
                .len()
                == 3
                && events["result"]["value"]["events"][2]["id"] == 1,
            "duplicate opening"
        );
        alter(
            &mut events,
            &json!({"command":"events"}),
            &Faults {
                gap: true,
                ..Faults::default()
            },
        )?;
        ensure!(
            events == json!({"id":1,"status":"failed","error":{"code":"gap"}}),
            "gap fault"
        );
        Ok(())
    }
    #[test]
    fn close_cancels_partial_front_handshake() -> Result<()> {
        let root = tempfile::tempdir()?;
        let mut p = Proxy::start(
            root.path(),
            &root.path().join("upstream"),
            &root.path().join("manager"),
            json!("instance"),
        )?;
        let mut client = UnixStream::connect(root.path().join("default.sock"))?;
        client.write_all(b"FUX")?;
        std::thread::sleep(Duration::from_millis(30));
        let start = Instant::now();
        p.close()?;
        ensure!(
            start.elapsed() < Duration::from_secs(1),
            "partial handshake prevented cleanup"
        );
        Ok(())
    }
}
