//! Owned socket adapter for binding/retirement fault injection.
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{
    io::{Read, Write},
    os::unix::net::UnixListener,
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
#[derive(Default)]
struct State {
    mode: String,
    arms: Vec<Value>,
    disarms: Vec<Value>,
}
pub struct Adapter {
    state: Arc<Mutex<State>>,
    stop: Arc<AtomicBool>,
    pub seen: Arc<AtomicBool>,
    pub release: Arc<AtomicBool>,
    pub done: Arc<AtomicBool>,
    worker: Option<JoinHandle<Result<()>>>,
}
impl Adapter {
    pub fn start(path: &Path, marker: String) -> Result<Self> {
        let listener = UnixListener::bind(path)?;
        listener.set_nonblocking(true)?;
        let state = Arc::new(Mutex::new(State {
            mode: "wrong".into(),
            ..State::default()
        }));
        let stop = Arc::new(AtomicBool::new(false));
        let seen = Arc::new(AtomicBool::new(false));
        let release = Arc::new(AtomicBool::new(false));
        let done = Arc::new(AtomicBool::new(false));
        let (s, c, h, r, d) = (
            state.clone(),
            stop.clone(),
            seen.clone(),
            release.clone(),
            done.clone(),
        );
        let worker = thread::spawn(move || -> Result<()> {
            while !c.load(Ordering::SeqCst) {
                let (mut peer, _) = match listener.accept() {
                    Ok(p) => p,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(10));
                        continue;
                    }
                    Err(e) => return Err(e.into()),
                };
                peer.set_nonblocking(true)?;
                let deadline = Instant::now() + Duration::from_secs(3);
                let mut data = Vec::new();
                loop {
                    ensure!(Instant::now() < deadline, "adapter request deadline");
                    if c.load(Ordering::SeqCst) {
                        return Ok(());
                    }
                    let mut buf = [0; 8192];
                    let n = match peer.read(&mut buf) {
                        Ok(n) => n,
                        Err(e)
                            if matches!(
                                e.kind(),
                                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                            ) =>
                        {
                            thread::sleep(Duration::from_millis(10));
                            continue;
                        }
                        Err(e) => return Err(e.into()),
                    };
                    ensure!(n > 0, "adapter request EOF");
                    data.extend_from_slice(&buf[..n]);
                    ensure!(data.len() <= 131072, "adapter request bound");
                    if data.contains(&b'\n') {
                        break;
                    }
                }
                let req: Value = serde_json::from_slice(&data)?;
                ensure!(req["v"] == 1 && req["marker"] == marker, "adapter identity");
                let mode = s
                    .lock()
                    .map_err(|_| anyhow::anyhow!("adapter mutex"))?
                    .mode
                    .clone();
                let mut reply = json!({"v":1,"producer":"socket-producer","status":"ready"});
                if req["op"] == "hello" {
                    match mode.as_str() {
                        "hello-wrong" => reply["producer"] = json!("other-lifetime"),
                        "hello-version" => reply["v"] = json!(2),
                        "hello-drop" => continue,
                        "hello-timeout" => {
                            thread::sleep(Duration::from_millis(2500));
                            d.store(true, Ordering::SeqCst);
                            continue;
                        }
                        "hello-stall" => {
                            h.store(true, Ordering::SeqCst);
                            let end = Instant::now() + Duration::from_secs(4);
                            while !r.load(Ordering::SeqCst) {
                                ensure!(Instant::now() < end, "probe fixture release deadline");
                                thread::sleep(Duration::from_millis(10));
                            }
                        }
                        _ => {}
                    }
                }
                if req["op"] == "arm" || req["op"] == "disarm" {
                    let arm = &req["prompt"];
                    let mut state = s.lock().map_err(|_| anyhow::anyhow!("adapter mutex"))?;
                    if req["op"] == "arm" {
                        state.arms.push(arm.clone())
                    } else {
                        state.disarms.push(arm.clone())
                    }
                    drop(state);
                    if mode == "drop" {
                        continue;
                    }
                    reply["status"] = json!(if req["op"] == "arm" {
                        "armed"
                    } else {
                        "disarmed"
                    });
                    reply["operation"] = arm["operation"].clone();
                    reply["input_operation"] = arm["input_operation"].clone();
                    reply["token"] = if mode == "wrong" {
                        json!("wrong")
                    } else {
                        arm["token"].clone()
                    };
                }
                peer.set_nonblocking(false)?;
                peer.set_write_timeout(Some(Duration::from_secs(3)))?;
                let mut bytes = serde_json::to_vec(&reply)?;
                bytes.push(b'\n');
                peer.write_all(&bytes)?;
            }
            Ok(())
        });
        Ok(Self {
            state,
            stop,
            seen,
            release,
            done,
            worker: Some(worker),
        })
    }
    pub fn mode(&self, mode: &str) -> Result<()> {
        self.state
            .lock()
            .map_err(|_| anyhow::anyhow!("adapter mutex"))?
            .mode = mode.into();
        Ok(())
    }
    pub fn arms(&self) -> Result<Vec<Value>> {
        Ok(self
            .state
            .lock()
            .map_err(|_| anyhow::anyhow!("adapter mutex"))?
            .arms
            .clone())
    }
    pub fn disarms(&self) -> Result<Vec<Value>> {
        Ok(self
            .state
            .lock()
            .map_err(|_| anyhow::anyhow!("adapter mutex"))?
            .disarms
            .clone())
    }
    pub fn close(&mut self) -> Result<()> {
        self.release.store(true, Ordering::SeqCst);
        self.stop.store(true, Ordering::SeqCst);
        if let Some(worker) = self.worker.take() {
            worker
                .join()
                .map_err(|_| anyhow::anyhow!("adapter thread panicked"))?
                .context("adapter fixture")?;
        }
        Ok(())
    }
}
impl Drop for Adapter {
    fn drop(&mut self) {
        let _ = self.close();
    }
}
