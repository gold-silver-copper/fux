//! Shared bounded callers and socket faults for the service integration scenario.
use crate::support::{
    contention::{self, RealClock},
    local::{Root, connect, until},
    process::{self, Guard, OwnedProcess},
    service,
};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{
    fs,
    io::{Read, Seek, SeekFrom, Write},
    net::Shutdown,
    os::{
        fd::OwnedFd,
        unix::net::{UnixListener, UnixStream},
    },
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
pub fn s(v: &Value) -> Result<&str> {
    v.as_str().context("string")
}
pub fn path(p: &Path) -> Result<&str> {
    p.to_str().context("path")
}
pub fn contains(v: &Value, text: &str) -> bool {
    v.to_string().contains(text)
}
pub fn items(v: &Value) -> Result<&Vec<Value>> {
    v.as_array().context("array")
}
pub fn altered(v: &Value, key: &str, value: Value) -> Value {
    let mut v = v.clone();
    v[key] = value;
    v
}
pub fn without(v: &Value, key: &str) -> Result<Value> {
    let mut v = v.clone();
    v.as_object_mut().context("object")?.remove(key);
    Ok(v)
}
pub struct Caller {
    pub child: Guard,
    out: fs::File,
    err: fs::File,
}
impl Caller {
    pub fn spawn(mut c: Command) -> Result<Self> {
        let out = tempfile::tempfile()?;
        let err = tempfile::tempfile()?;
        let child = Guard(
            c.stdout(out.try_clone()?)
                .stderr(err.try_clone()?)
                .spawn()?,
        );
        Ok(Self { child, out, err })
    }
    pub fn finish(&mut self, timeout: Duration) -> Result<process::Output> {
        let status = process::wait(&mut self.child.0, timeout)?;
        let read = |f: &mut fs::File| -> Result<Vec<u8>> {
            f.seek(SeekFrom::Start(0))?;
            let mut b = Vec::new();
            f.take(2097153).read_to_end(&mut b)?;
            ensure!(b.len() <= 2097152, "caller output bound");
            Ok(b)
        };
        Ok(process::Output {
            status,
            stdout: read(&mut self.out)?,
            stderr: read(&mut self.err)?,
        })
    }
    pub fn value(&mut self, timeout: Duration) -> Result<Value> {
        let r = self.finish(timeout)?;
        ensure!(
            r.status.success(),
            "caller: {}",
            String::from_utf8_lossy(&r.stderr)
        );
        Ok(serde_json::from_slice(&r.stdout)?)
    }
    pub fn stop(&mut self, force: bool) -> Result<()> {
        if self.child.0.try_wait()?.is_none() {
            if force {
                self.child.0.kill()?;
            } else {
                self.child.0.terminate()?;
            }
            process::wait(&mut self.child.0, Duration::from_secs(8))?;
        }
        Ok(())
    }
}
pub struct Harness<'a> {
    pub root: &'a Root,
    pub zor: &'a Path,
    pub endpoint: PathBuf,
    pub instance: Value,
    pub handle: Value,
    pub journal: PathBuf,
}
impl Harness<'_> {
    pub fn snapshot(&self) -> Result<Value> {
        self.exchange(&json!({"v":1,"id":7,"op":"snapshot"}))
    }
    pub fn exchange(&self, v: &Value) -> Result<Value> {
        service::rpc(&self.endpoint, v, Duration::from_secs(3))
    }
    pub fn api_instance(&self, payload: Value, ok: bool, instance: &Value) -> Result<Value> {
        let replay = matches!(
            s(&payload["action"])?,
            "worktree-create"
                | "worktree-inspect"
                | "worktree-reconcile"
                | "worktree-list"
                | "list"
                | "start"
                | "adopt"
                | "adapter-status"
                | "worktree-remove"
                | "forget"
                | "stop"
                | "inspect"
                | "require-artifact"
                | "result"
                | "artifact-collect"
                | "artifact-inspect"
                | "check"
                | "require-check"
                | "check-inspect"
                | "source-collect"
                | "source-inspect"
                | "source-file"
                | "verify"
                | "changes-collect"
                | "changes-inspect"
                | "prepare"
                | "submit"
                | "reconcile"
                | "report"
                | "wait"
                | "cancel"
                | "reserve"
                | "abandon"
                | "launch-reconcile"
        );
        let req = json!({"v":1,"id":41,"op":"task","service_instance":instance,"task":payload});
        let reply = contention::retry_busy(
            |left| service::rpc(&self.endpoint, &req, left.min(Duration::from_secs(3))),
            |r| contention::api_busy(r, &req),
            Instant::now() + Duration::from_secs(12),
            replay,
            &RealClock,
        )?;
        ensure!(
            (reply["status"] == "completed") == ok,
            "task {payload}: {reply}"
        );
        Ok(if ok { reply["value"].clone() } else { reply })
    }
    pub fn api(&self, payload: Value, ok: bool) -> Result<Value> {
        self.api_instance(payload, ok, &self.instance)
    }
    pub fn fux(&self, command: &str, mut fields: Value) -> Result<Value> {
        fields["id"] = json!(1);
        fields["command"] = json!(command);
        fields["instance"] = self.handle["instance"].clone();
        crate::support::local::completed(&self.root.control(), fields)
    }
    pub fn command(&self, args: &[&str]) -> Command {
        let mut c = self.root.command(self.zor);
        c.args(args).stdin(Stdio::null());
        c
    }
    pub fn cli(&self, args: &[&str], ok: bool, timeout: Duration) -> Result<Value> {
        let r = process::output(self.command(args), timeout, 2097152)?;
        ensure!(
            r.status.success() == ok,
            "CLI {args:?}: {}",
            String::from_utf8_lossy(&r.stderr)
        );
        Ok(if ok {
            serde_json::from_slice(&r.stdout)?
        } else {
            json!(String::from_utf8(r.stderr)?)
        })
    }
    pub fn service_command(&self) -> Result<Command> {
        Ok(self.command(&[
            "--state-directory",
            path(&self.root.path().join("tasks"))?,
            "--rules",
            path(&self.root.path().join("rules"))?,
            "--agent",
            "test",
            "serve",
        ]))
    }
    pub fn adopt(&self, id: &str, title: &str, pane: &Value) -> Value {
        json!({"action":"adopt","id":id,"title":title,"instance":self.handle["instance"],"workspace":self.handle["workspace"],"pane":pane})
    }
    pub fn closed(&self, pane: &Value) -> Result<()> {
        until(Duration::from_secs(12), || {
            let l = self.fux("list", json!({}))?;
            for w in items(&l["workspaces"])? {
                for t in items(&w["tabs"])? {
                    for p in items(&t["panes"])? {
                        if &p["id"] == pane {
                            return Ok(None);
                        }
                    }
                }
            }
            Ok(Some(()))
        })
    }
    pub fn stop_task(&self, id: &str, caller: u64) -> Result<Value> {
        until(Duration::from_secs(12), || {
            let r=self.exchange(&json!({"v":1,"id":caller,"op":"task","service_instance":self.instance,"task":{"action":"stop","id":id}}))?;
            Ok((r["status"] == "completed").then(|| r["value"].clone()))
        })
    }
    pub fn delivered(&self, op: &str) -> Result<Value> {
        until(Duration::from_secs(12), || {
            let v = self.api(json!({"action":"reconcile","operation":op}), true)?;
            Ok((v["delivery"] == "delivered").then_some(v))
        })
    }
    pub fn write(&self, v: &Value) -> Result<Vec<u8>> {
        let bytes = serde_json::to_vec(v)?;
        fs::write(&self.journal, &bytes)?;
        Ok(bytes)
    }
}
pub fn state(v: &Value) -> &Value {
    &v["snapshot"]["observations"][0]["state"]
}
pub fn line(peer: &mut UnixStream, limit: usize, timeout: Duration) -> Result<Value> {
    peer.set_nonblocking(true)?;
    let end = Instant::now() + timeout;
    let mut bytes = Vec::new();
    loop {
        ensure!(Instant::now() < end, "socket line deadline");
        let mut buf = [0; 8192];
        let n = match peer.read(&mut buf) {
            Ok(n) => n,
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                ) =>
            {
                thread::sleep(Duration::from_millis(5));
                continue;
            }
            Err(e) => return Err(e.into()),
        };
        ensure!(n > 0, "socket line EOF");
        bytes.extend_from_slice(&buf[..n]);
        ensure!(bytes.len() <= limit, "socket line bound");
        if bytes.contains(&b'\n') {
            return Ok(serde_json::from_slice(&bytes)?);
        }
    }
}
pub fn raw(endpoint: &Path, bytes: &[u8]) -> Result<Value> {
    let mut peer = connect(endpoint, Instant::now() + Duration::from_secs(3))?;
    peer.set_write_timeout(Some(Duration::from_secs(3)))?;
    peer.write_all(bytes)?;
    line(&mut peer, 524288, Duration::from_secs(3))
}
pub fn daemon(h: &Harness<'_>) -> Result<(Caller, UnixStream)> {
    let (parent, child) = UnixStream::pair()?;
    let mut c = h.service_command()?;
    c.arg("--daemon-child")
        .stdin(Stdio::from(OwnedFd::from(child)));
    Ok((Caller::spawn(c)?, parent))
}
pub fn activate(mut peer: UnixStream) -> Result<()> {
    peer.shutdown(Shutdown::Read)?;
    peer.set_nonblocking(false)?;
    peer.set_write_timeout(Some(Duration::from_secs(3)))?;
    peer.write_all(b"activate\n")?;
    Ok(())
}
pub struct Background(pub PathBuf);
impl Background {
    pub fn close(&self) -> Result<()> {
        let peer = match connect(&self.0, Instant::now() + Duration::from_secs(3)) {
            Ok(p) => p,
            Err(e)
                if e.downcast_ref::<nix::errno::Errno>().is_some_and(|e| {
                    matches!(
                        e,
                        nix::errno::Errno::ENOENT | nix::errno::Errno::ECONNREFUSED
                    )
                }) =>
            {
                return Ok(());
            }
            Err(e) => return Err(e),
        };
        #[cfg(target_os = "macos")]
        let pid = nix::sys::socket::getsockopt(&peer, nix::sys::socket::sockopt::LocalPeerPid)?;
        #[cfg(target_os = "linux")]
        let pid = {
            let c =
                nix::sys::socket::getsockopt(&peer, nix::sys::socket::sockopt::PeerCredentials)?;
            ensure!(c.uid() == nix::unistd::getuid().as_raw(), "foreign daemon");
            c.pid()
        };
        ensure!(
            pid > 1 && pid != i32::try_from(std::process::id())?,
            "invalid daemon peer PID"
        );
        nix::sys::signal::kill(
            nix::unistd::Pid::from_raw(pid),
            nix::sys::signal::Signal::SIGTERM,
        )?;
        drop(peer);
        until(Duration::from_secs(12), || {
            Ok((!self.0.exists()).then_some(()))
        })
    }
}
impl Drop for Background {
    fn drop(&mut self) {
        let _ = self.close();
    }
}
pub struct Proxy {
    pub block: Arc<AtomicBool>,
    pub blocked: Arc<AtomicBool>,
    pub release: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<Result<()>>>,
}
impl Proxy {
    pub fn new(endpoint: &Path, control: PathBuf) -> Result<Self> {
        let listener = UnixListener::bind(endpoint)?;
        listener.set_nonblocking(true)?;
        let block = Arc::new(AtomicBool::new(false));
        let blocked = Arc::new(AtomicBool::new(false));
        let release = Arc::new(AtomicBool::new(false));
        let stop = Arc::new(AtomicBool::new(false));
        let (b, d, r, s) = (
            block.clone(),
            blocked.clone(),
            release.clone(),
            stop.clone(),
        );
        let worker = thread::spawn(move || -> Result<()> {
            while !s.load(Ordering::SeqCst) {
                let (mut peer, _) = match listener.accept() {
                    Ok(p) => p,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(10));
                        continue;
                    }
                    Err(e) => return Err(e.into()),
                };
                peer.set_nonblocking(false)?;
                peer.set_read_timeout(Some(Duration::from_secs(3)))?;
                peer.set_write_timeout(Some(Duration::from_secs(3)))?;
                let mut preface = [0; 8];
                peer.read_exact(&mut preface)?;
                ensure!(&preface == b"FUXCTL3\n", "proxy preface");
                peer.write_all(&preface)?;
                let req = line(&mut peer, 1048576, Duration::from_secs(3))?;
                if req["command"] == "list" && b.swap(false, Ordering::SeqCst) {
                    d.store(true, Ordering::SeqCst);
                    until(Duration::from_secs(3), || {
                        Ok(r.load(Ordering::SeqCst).then_some(()))
                    })?;
                }
                let reply = crate::support::local::rpc(&control, req)?;
                let mut bytes = serde_json::to_vec(&reply)?;
                bytes.push(b'\n');
                peer.set_nonblocking(false)?;
                peer.write_all(&bytes)?;
            }
            Ok(())
        });
        Ok(Self {
            block,
            blocked,
            release,
            stop,
            worker: Some(worker),
        })
    }
    pub fn close(&mut self) -> Result<()> {
        self.release.store(true, Ordering::SeqCst);
        self.stop.store(true, Ordering::SeqCst);
        if let Some(worker) = self.worker.take() {
            worker
                .join()
                .map_err(|_| anyhow::anyhow!("service proxy panic"))??;
        }
        Ok(())
    }
}
impl Drop for Proxy {
    fn drop(&mut self) {
        let _ = self.close();
    }
}
pub fn descendant(args: &[String]) -> Result<()> {
    let tree = Path::new(args.get(1).context("tree")?);
    let marker = Path::new(args.get(2).context("marker")?);
    let depth = args.get(3).context("depth")?.parse::<u8>()?;
    let mut command = if depth > 0 {
        let mut c = Command::new(std::env::current_exe()?);
        c.args(["fixture-worker", "service-descendant"])
            .arg(tree)
            .arg(marker)
            .arg((depth - 1).to_string());
        c
    } else {
        let mut c = Command::new("/bin/cat");
        c.current_dir(tree).stdin(Stdio::piped());
        c
    };
    let mut child = Guard(command.spawn()?);
    if depth == 0 {
        fs::write(
            marker,
            std::env::current_dir()?.as_os_str().as_encoded_bytes(),
        )?;
    }
    process::wait(&mut child.0, Duration::from_secs(90))?;
    Ok(())
}
