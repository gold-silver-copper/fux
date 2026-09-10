//! Owned local fixture: forward creation, discard reply, count every creation request.
use anyhow::{Context, Result, ensure};
use serde_json::Value;
use std::{
    fs,
    io::{Read, Write},
    os::unix::net::{UnixListener, UnixStream},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
fn line(peer: &mut UnixStream, limit: usize) -> Result<Vec<u8>> {
    let end = Instant::now() + Duration::from_secs(3);
    let mut bytes = Vec::new();
    loop {
        peer.set_read_timeout(Some(
            end.checked_duration_since(Instant::now())
                .context("proxy read deadline")?,
        ))?;
        let mut b = [0];
        peer.read_exact(&mut b)?;
        bytes.push(b[0]);
        ensure!(bytes.len() <= limit, "proxy frame bound");
        if b[0] == b'\n' {
            return Ok(bytes);
        }
    }
}
pub struct Proxy {
    stop: Arc<AtomicBool>,
    count: Arc<AtomicUsize>,
    thread: Option<JoinHandle<Result<()>>>,
    front: PathBuf,
    actual: PathBuf,
}
impl Proxy {
    pub fn start(front: &Path) -> Result<Self> {
        let actual = front.with_file_name("actual.sock");
        fs::rename(front, &actual)?;
        let listener = match UnixListener::bind(front) {
            Ok(v) => v,
            Err(e) => {
                fs::rename(&actual, front)?;
                return Err(e.into());
            }
        };
        listener.set_nonblocking(true)?;
        let stop = Arc::new(AtomicBool::new(false));
        let count = Arc::new(AtomicUsize::new(0));
        let stopped = stop.clone();
        let requests = count.clone();
        let upstream = actual.clone();
        let thread = thread::spawn(move || -> Result<()> {
            while !stopped.load(Ordering::Acquire) {
                let mut peer = match listener.accept() {
                    Ok((p, _)) => p,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(10));
                        continue;
                    }
                    Err(e) => return Err(e.into()),
                };
                peer.set_nonblocking(false)?;
                peer.set_write_timeout(Some(Duration::from_secs(3)))?;
                ensure!(line(&mut peer, 4)? == b"FUX\n", "proxy preface");
                peer.write_all(b"FUX\n")?;
                let request: Value = serde_json::from_slice(&line(&mut peer, 1048576)?)?;
                let reply = crate::runtime::raw_rpc(&upstream, request.clone())?;
                if request["command"] == "split" {
                    requests.fetch_add(1, Ordering::AcqRel);
                    continue;
                }
                let mut bytes = serde_json::to_vec(&reply)?;
                bytes.push(b'\n');
                peer.write_all(&bytes)?;
            }
            Ok(())
        });
        Ok(Self {
            stop,
            count,
            thread: Some(thread),
            front: front.to_owned(),
            actual,
        })
    }
    pub fn count(&self) -> usize {
        self.count.load(Ordering::Acquire)
    }
    pub fn close(&mut self) -> Result<()> {
        self.stop.store(true, Ordering::Release);
        let result = if let Some(t) = self.thread.take() {
            let end = Instant::now() + Duration::from_secs(10);
            while !t.is_finished() {
                ensure!(Instant::now() < end, "proxy did not stop");
                thread::sleep(Duration::from_millis(10));
            }
            t.join().map_err(|_| anyhow::anyhow!("proxy panic"))?
        } else {
            Ok(())
        };
        if self.actual.exists() {
            fs::remove_file(&self.front)?;
            fs::rename(&self.actual, &self.front)?;
        }
        result
    }
}
impl Drop for Proxy {
    fn drop(&mut self) {
        if let Err(e) = self.close() {
            eprintln!("resume proxy cleanup: {e:#}");
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn forwards_requests_and_drops_only_creation_reply() -> Result<()> {
        let root = tempfile::tempdir()?;
        let front = root.path().join("default.sock");
        let listener = UnixListener::bind(&front)?;
        listener.set_nonblocking(true)?;
        let backend = thread::spawn(move || -> Result<()> {
            for command in ["split", "list"] {
                let end = Instant::now() + Duration::from_secs(5);
                let mut peer = loop {
                    match listener.accept() {
                        Ok((p, _)) => break p,
                        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                        Err(e) => return Err(e.into()),
                    }
                    ensure!(Instant::now() < end, "fixture accept deadline");
                    thread::sleep(Duration::from_millis(10));
                };
                peer.set_nonblocking(false)?;
                peer.set_write_timeout(Some(Duration::from_secs(3)))?;
                ensure!(line(&mut peer, 4)? == b"FUX\n", "upstream preface");
                peer.write_all(b"FUX\n")?;
                let request: Value = serde_json::from_slice(&line(&mut peer, 1048576)?)?;
                ensure!(request["command"] == command, "forwarded request");
                let reply = json!({"id":request["id"],"status":"completed","result":{"value":{"command":command}}});
                writeln!(peer, "{reply}")?;
            }
            Ok(())
        });
        let mut proxy = Proxy::start(&front)?;
        let result = (|| -> Result<()> {
            ensure!(
                crate::runtime::raw_rpc(
                    &front,
                    json!({"id":1,"command":"split","axis":"horizontal"})
                )
                .is_err(),
                "creation reply not dropped"
            );
            ensure!(proxy.count() == 1, "creation not forwarded/count mismatch");
            ensure!(
                crate::runtime::rpc(&front, json!({"id":2,"command":"list"}))?
                    == json!({"command":"list"}),
                "ordinary response changed"
            );
            Ok(())
        })();
        let cleanup = proxy.close();
        let backend = backend
            .join()
            .map_err(|_| anyhow::anyhow!("fixture backend panic"))?;
        cleanup?;
        backend?;
        result
    }
}
