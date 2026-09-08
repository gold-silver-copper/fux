//! Observe real viewer input ordering and acknowledgment through a private server.
use crate::support::{
    attachment::{connect, receive, send},
    local::{Root, until},
    process::{Guard, wait},
};
use anyhow::{Result, ensure};
use serde_json::json;
use std::{
    fs,
    io::Read,
    os::unix::{fs::PermissionsExt, net::UnixListener},
    path::Path,
    process::Stdio,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

struct Drain {
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<Result<()>>>,
}
impl Drain {
    fn start(fd: std::os::fd::OwnedFd) -> Result<Self> {
        nix::fcntl::fcntl(
            &fd,
            nix::fcntl::FcntlArg::F_SETFL(nix::fcntl::OFlag::O_NONBLOCK),
        )?;
        let stop = Arc::new(AtomicBool::new(false));
        let stopping = stop.clone();
        let thread = std::thread::spawn(move || {
            let mut bytes = [0; 65536];
            while !stopping.load(Ordering::Relaxed) {
                match nix::unistd::read(&fd, &mut bytes) {
                    Ok(0) | Err(nix::errno::Errno::EIO) => break,
                    Ok(_) | Err(nix::errno::Errno::EAGAIN) => {}
                    Err(error) => return Err(error.into()),
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            Ok(())
        });
        Ok(Self {
            stop,
            thread: Some(thread),
        })
    }
    fn finish(&mut self) -> Result<()> {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            thread
                .join()
                .map_err(|_| anyhow::anyhow!("terminal drain panicked"))??;
        }
        Ok(())
    }
}
impl Drop for Drain {
    fn drop(&mut self) {
        if let Err(error) = self.finish() {
            eprintln!("terminal drain: {error:#}");
        }
    }
}

pub(super) fn run(binary: &Path) -> Result<()> {
    let root = Root::new("fdrain-rs-", &["/bin/sh".into()])?;
    let mut server = root.server(binary)?;
    let mut source = connect(&root.path().join("fux/default.attach.sock"))?;
    send(
        &mut source,
        &json!({"type":"hello","version":6,"rows":12,"columns":48}),
    )?;
    ensure!(
        receive(&mut source)? == json!({"hello":{"version":6}}),
        "source hello"
    );
    let state = receive(&mut source)?;
    ensure!(state.get("state").is_some(), "source state missing");
    drop(source);
    let path = root.path().join("probe.sock");
    let listener = UnixListener::bind(&path)?;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
    listener.set_nonblocking(true)?;
    let pty = crate::support::pty::open(12, 48)?;
    let mut drain = Drain::start(pty.master.try_clone()?)?;
    let mut child = Guard(
        root.command(binary)
            .args(["attach", "--socket"])
            .arg(&path)
            .stdin(Stdio::from(pty.slave.try_clone()?))
            .stdout(Stdio::from(pty.slave.try_clone()?))
            .stderr(Stdio::from(pty.slave.try_clone()?))
            .spawn()?,
    );
    let (mut peer, _) = until(Duration::from_secs(5), || match listener.accept() {
        Ok(value) => Ok(Some(value)),
        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => Ok(None),
        Err(error) => Err(error.into()),
    })?;
    peer.set_nonblocking(false)?;
    peer.set_read_timeout(Some(Duration::from_secs(5)))?;
    peer.set_write_timeout(Some(Duration::from_secs(5)))?;
    ensure!(receive(&mut peer)?["type"] == "hello", "viewer hello");
    send(&mut peer, &json!({"hello":{"version":6}}))?;
    ensure!(receive(&mut peer)?["type"] == "resize", "viewer resize");
    crate::support::attachment::send_server(&mut peer, &state)?;
    let previous = b"PREVIOUS_CHUNK";
    ensure!(
        nix::unistd::write(&pty.master, previous)? == previous.len(),
        "short input write"
    );
    let mut data = Vec::new();
    while data != previous {
        let message = receive(&mut peer)?;
        ensure!(message["type"] == "input", "wrong input frame: {message}");
        let chunk: Vec<u8> = serde_json::from_value(message["bytes"].clone())?;
        ensure!(!chunk.is_empty(), "empty input frame");
        data.extend(chunk);
        ensure!(previous.starts_with(&data), "input changed: {data:?}");
    }
    ensure!(
        nix::unistd::write(&pty.master, b"\x01d\x01t")? == 4,
        "short detach write"
    );
    ensure!(
        receive(&mut peer)? == json!({"type":"detach"}),
        "wrong detach frame"
    );
    std::thread::sleep(Duration::from_millis(200));
    ensure!(
        child.0.try_wait()?.is_none(),
        "viewer exited before detach acknowledgment"
    );
    peer.set_nonblocking(true)?;
    let mut byte = [0];
    ensure!(
        matches!(peer.read(&mut byte), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock),
        "trailing bytes or EOF after detach"
    );
    peer.set_nonblocking(false)?;
    send(&mut peer, &json!({"exited":{"code":null}}))?;
    ensure!(
        wait(&mut child.0, Duration::from_secs(5))?.success(),
        "viewer exit failed"
    );
    ensure!(
        peer.read(&mut byte)? == 0,
        "viewer retained connection after exit"
    );
    drain.finish()?;
    server.finish()?;
    println!(
        "PASS separate-read detach sends preceding input, waits for exit and drops the suffix"
    );
    Ok(())
}
