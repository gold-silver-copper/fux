//! Task-owned manager proxy with explicit reply gates; ordinary RPCs keep flowing.
use anyhow::{Context, Result, ensure};
use serde_json::Value;
use std::{
    collections::VecDeque,
    fs,
    io::{Read, Write},
    os::unix::{
        fs::PermissionsExt,
        net::{UnixListener, UnixStream},
    },
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::Duration,
};

pub struct Pending {
    pub request: Value,
    release: mpsc::Sender<Option<Value>>,
}
impl Pending {
    /// None forwards the actual request; Some supplies a controlled reply.
    pub fn finish(self, reply: Option<Value>) -> Result<()> {
        self.release
            .send(reply)
            .context("release delayed manager reply")
    }
}
pub struct Proxy {
    path: PathBuf,
    upstream: PathBuf,
    stopped: Arc<AtomicBool>,
    armed: Arc<Mutex<VecDeque<String>>>,
    pending: mpsc::Receiver<Pending>,
    worker: Option<thread::JoinHandle<()>>,
}
impl Proxy {
    pub fn start(path: &Path) -> Result<Self> {
        let upstream = path.with_file_name("manager-delay-upstream.sock");
        ensure!(!upstream.exists(), "proxy upstream already exists");
        fs::rename(path, &upstream)?;
        let listener = match UnixListener::bind(path) {
            Ok(listener) => listener,
            Err(error) => {
                fs::rename(&upstream, path)?;
                return Err(error.into());
            }
        };
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
        listener.set_nonblocking(true)?;
        let stopped = Arc::new(AtomicBool::new(false));
        let armed = Arc::new(Mutex::new(VecDeque::<String>::new()));
        let (tx, pending) = mpsc::channel();
        let stop = stopped.clone();
        let arms = armed.clone();
        let source = upstream.clone();
        let worker = thread::spawn(move || {
            let mut handlers = Vec::new();
            while !stop.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((peer, _)) => {
                        let (source, stop, arms, tx) =
                            (source.clone(), stop.clone(), arms.clone(), tx.clone());
                        handlers.push(thread::spawn(move || {
                            if let Err(error) = serve(peer, &source, &stop, &arms, &tx)
                                && !stop.load(Ordering::Relaxed)
                            {
                                eprintln!("manager delay proxy: {error:#}");
                            }
                        }));
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5))
                    }
                    Err(_) => break,
                }
            }
            for handler in handlers {
                let _ = handler.join();
            }
        });
        Ok(Self {
            path: path.into(),
            upstream,
            stopped,
            armed,
            pending,
            worker: Some(worker),
        })
    }
    pub fn arm(&self, request: &str) -> Result<()> {
        self.armed
            .lock()
            .map_err(|_| anyhow::anyhow!("proxy lock poisoned"))?
            .push_back(request.into());
        Ok(())
    }
    pub fn take(&self) -> Option<Pending> {
        self.pending.try_recv().ok()
    }
}
impl Drop for Proxy {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::Relaxed);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
        let _ = fs::remove_file(&self.path);
        let _ = fs::rename(&self.upstream, &self.path);
    }
}
fn serve(
    mut peer: UnixStream,
    upstream: &Path,
    stopped: &AtomicBool,
    armed: &Mutex<VecDeque<String>>,
    pending: &mpsc::Sender<Pending>,
) -> Result<()> {
    peer.set_nonblocking(false)?;
    peer.set_read_timeout(Some(Duration::from_secs(2)))?;
    peer.set_write_timeout(Some(Duration::from_secs(2)))?;
    let mut preface = [0; 4];
    peer.read_exact(&mut preface)
        .context("manager client preface")?;
    ensure!(&preface == b"FUX\n", "manager preface");
    peer.write_all(b"FUX\n")?;
    let mut bytes = Vec::new();
    loop {
        let mut byte = [0];
        peer.read_exact(&mut byte)
            .context("manager client request")?;
        if byte[0] == b'\n' {
            break;
        }
        bytes.push(byte[0]);
        ensure!(bytes.len() <= 1024 * 1024, "manager fixture bound");
    }
    let request: Value = serde_json::from_slice(&bytes)?;
    let gate = {
        let mut queue = armed
            .lock()
            .map_err(|_| anyhow::anyhow!("proxy lock poisoned"))?;
        if queue
            .front()
            .is_some_and(|kind| request["request"].as_str() == Some(kind))
        {
            queue.pop_front();
            true
        } else {
            false
        }
    };
    let override_reply = if gate {
        let (release, receive) = mpsc::channel();
        pending.send(Pending {
            request: request.clone(),
            release,
        })?;
        loop {
            ensure!(!stopped.load(Ordering::Relaxed), "fixture stopped");
            match receive.recv_timeout(Duration::from_millis(20)) {
                Ok(reply) => break reply,
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(error) => return Err(error.into()),
            }
        }
    } else {
        None
    };
    let reply = match override_reply {
        Some(reply) => reply,
        None => super::local::rpc(upstream, request)?,
    };
    serde_json::to_writer(&mut peer, &reply)?;
    peer.write_all(b"\n")?;
    Ok(())
}
