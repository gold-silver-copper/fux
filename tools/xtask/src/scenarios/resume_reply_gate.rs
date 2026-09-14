//! Drop one completed resume reply after the real service has committed it.
use anyhow::{Context, Result, ensure};
use serde_json::Value;
use std::{
    io::{BufRead, BufReader, Read, Write},
    os::unix::{
        fs::PermissionsExt,
        net::{UnixListener, UnixStream},
    },
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

pub(super) struct Gate {
    stop: Arc<AtomicBool>,
    mutations: Arc<AtomicUsize>,
    dropped: Arc<AtomicBool>,
    thread: Option<JoinHandle<Result<()>>>,
}
impl Gate {
    pub fn start(path: &Path, upstream: &Path) -> Result<Self> {
        let listener = UnixListener::bind(path)?;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
        listener.set_nonblocking(true)?;
        let stop = Arc::new(AtomicBool::new(false));
        let mutations = Arc::new(AtomicUsize::new(0));
        let dropped = Arc::new(AtomicBool::new(false));
        let (done, count, lost) = (stop.clone(), mutations.clone(), dropped.clone());
        let upstream = upstream.to_owned();
        let thread = thread::spawn(move || {
            let mut workers: Vec<JoinHandle<Result<()>>> = Vec::new();
            let result = (|| -> Result<()> {
                while !done.load(Ordering::Acquire) {
                    let mut index = 0;
                    while index < workers.len() {
                        if workers[index].is_finished() {
                            workers
                                .swap_remove(index)
                                .join()
                                .map_err(|_| anyhow::anyhow!("gate handler panicked"))??;
                        } else {
                            index += 1;
                        }
                    }
                    let (peer, _) = match listener.accept() {
                        Ok(peer) => peer,
                        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                            thread::sleep(Duration::from_millis(10));
                            continue;
                        }
                        Err(e) => return Err(e.into()),
                    };
                    ensure!(workers.len() < 32, "gate connection bound");
                    let (upstream, count, lost) = (upstream.clone(), count.clone(), lost.clone());
                    workers.push(thread::spawn(move || {
                        handle(peer, &upstream, &count, &lost)
                    }));
                }
                Ok(())
            })();
            let mut failure = result.err();
            for worker in workers {
                let result = worker
                    .join()
                    .map_err(|_| anyhow::anyhow!("gate handler panicked"))
                    .and_then(|result| result);
                if failure.is_none() {
                    failure = result.err();
                }
            }
            failure.map_or(Ok(()), Err)
        });
        Ok(Self {
            stop,
            mutations,
            dropped,
            thread: Some(thread),
        })
    }
    pub fn verify(&self, mutations: usize) -> Result<()> {
        ensure!(
            self.dropped.load(Ordering::Acquire),
            "no completed reply was dropped"
        );
        ensure!(
            self.mutations.load(Ordering::Acquire) == mutations,
            "unexpected mutation replay"
        );
        Ok(())
    }
    pub fn check(&mut self) -> Result<()> {
        if self
            .thread
            .as_ref()
            .is_some_and(|thread| thread.is_finished())
        {
            self.finish()?;
        }
        Ok(())
    }
    pub fn finish(&mut self) -> Result<()> {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            thread
                .join()
                .map_err(|_| anyhow::anyhow!("reply gate panicked"))?
                .context("resume reply gate")?;
        }
        Ok(())
    }
}
impl Drop for Gate {
    fn drop(&mut self) {
        let _ = self.finish();
    }
}

fn handle(
    mut peer: UnixStream,
    upstream: &Path,
    count: &AtomicUsize,
    lost: &AtomicBool,
) -> Result<()> {
    peer.set_read_timeout(Some(Duration::from_secs(5)))?;
    peer.set_write_timeout(Some(Duration::from_secs(5)))?;
    let mut bytes = Vec::new();
    let read = BufReader::new(peer.try_clone()?)
        .take(524289)
        .read_until(b'\n', &mut bytes);
    if bytes.is_empty()
        && (read.is_ok()
            || read.as_ref().is_err_and(|error| {
                matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                )
            }))
    {
        return Ok(()); // A local peer can disappear without sending a request.
    }
    read.context("read gate request")?;
    ensure!(
        bytes.len() <= 524288 && bytes.ends_with(b"\n"),
        "gate request bound/framing"
    );
    let request: Value = serde_json::from_slice(&bytes)?;
    let mutation = request
        .pointer("/task/action")
        .is_some_and(|value| value == "guarded-resume");
    if mutation {
        count.fetch_add(1, Ordering::AcqRel);
    }
    let response = crate::support::service::rpc(upstream, &request, Duration::from_secs(10))
        .context("read upstream reply")?;
    if mutation && !lost.swap(true, Ordering::AcqRel) {
        ensure!(
            response["status"] == "completed",
            "gate cannot drop an uncommitted reply: {response}"
        );
        return Ok(());
    }
    let mut bytes = serde_json::to_vec(&response)?;
    ensure!(bytes.len() <= 524288, "gate response bound");
    bytes.push(b'\n');
    peer.write_all(&bytes).context("write gate reply")?;
    Ok(())
}
