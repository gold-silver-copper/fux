//! Isolated capture processes and bounded public fux requests; no production dependency.
use anyhow::{Context, Result, ensure};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    fs,
    io::{Read, Seek, SeekFrom, Write},
    os::{
        fd::{AsFd, AsRawFd},
        unix::{fs::PermissionsExt, net::UnixStream},
    },
    path::Path,
    process::{Child, Command, ExitStatus, Output, Stdio},
    time::{Duration, Instant},
};

pub fn hash(path: &Path) -> Result<String> {
    use sha2::{Digest, Sha256};
    let mut file = fs::File::open(path)?;
    let mut hash = Sha256::new();
    let mut bytes = [0; 65536];
    loop {
        let count = file.read(&mut bytes)?;
        if count == 0 {
            break;
        }
        hash.update(&bytes[..count]);
    }
    Ok(format!("{:x}", hash.finalize()))
}
pub struct Owner(pub Child);
impl Owner {
    pub fn wait(&mut self, timeout: Duration) -> Result<ExitStatus> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(status) = self.0.try_wait()? {
                return Ok(status);
            }
            ensure!(Instant::now() < deadline, "owned capture child deadline");
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    pub fn stop(&mut self) -> Result<ExitStatus> {
        if self.0.try_wait()?.is_none() {
            nix::sys::signal::kill(
                nix::unistd::Pid::from_raw(i32::try_from(self.0.id())?),
                nix::sys::signal::Signal::SIGTERM,
            )?;
        }
        match self.wait(Duration::from_secs(10)) {
            Ok(status) => Ok(status),
            Err(error) => {
                self.0.kill()?;
                self.wait(Duration::from_secs(5))?;
                Err(error)
            }
        }
    }
}
impl Drop for Owner {
    fn drop(&mut self) {
        if self.0.try_wait().is_ok_and(|s| s.is_none())
            && let Err(error) = self.stop()
        {
            eprintln!("capture cleanup: {error:#}");
        }
    }
}
pub struct Root {
    pub directory: tempfile::TempDir,
    env: BTreeMap<String, String>,
}
impl Root {
    pub fn set_env(&mut self, key: &str, value: String) {
        self.env.insert(key.into(), value);
    }
    pub fn new(prefix: &str) -> Result<Self> {
        let directory = tempfile::Builder::new()
            .prefix(prefix)
            .permissions(fs::Permissions::from_mode(0o700))
            .tempdir_in("/tmp")?;
        for path in [
            "config/fux",
            "state",
            "cache",
            "data",
            "codex",
            "claude",
            "work",
        ] {
            fs::create_dir_all(directory.path().join(path))?;
        }
        let mut env: BTreeMap<String, String> = [
            ("PATH", "/usr/bin:/bin:/usr/sbin:/sbin:/opt/homebrew/bin"),
            ("SHELL", "/bin/sh"),
            ("TERM", "xterm-256color"),
            ("LANG", "en_US.UTF-8"),
        ]
        .into_iter()
        .map(|(k, v)| (k.into(), v.into()))
        .collect();
        for (key, path) in [
            ("HOME", ""),
            ("XDG_RUNTIME_DIR", ""),
            ("XDG_CONFIG_HOME", "config"),
            ("XDG_STATE_HOME", "state"),
            ("XDG_CACHE_HOME", "cache"),
            ("XDG_DATA_HOME", "data"),
            ("CODEX_HOME", "codex"),
            ("CLAUDE_CONFIG_DIR", "claude"),
        ] {
            env.insert(
                key.into(),
                directory
                    .path()
                    .join(path)
                    .to_str()
                    .context("capture root UTF-8")?
                    .into(),
            );
        }
        Ok(Self { directory, env })
    }
    pub fn path(&self) -> &Path {
        self.directory.path()
    }
    pub fn command(&self, binary: &Path) -> Command {
        let mut c = Command::new(binary);
        c.env_clear()
            .envs(&self.env)
            .current_dir(self.path().join("work"));
        c
    }
}
pub fn output(mut command: Command, timeout: Duration) -> Result<Output> {
    let mut stdout = tempfile::tempfile()?;
    let mut stderr = tempfile::tempfile()?;
    let mut child = Owner(
        command
            .stdin(Stdio::null())
            .stdout(stdout.try_clone()?)
            .stderr(stderr.try_clone()?)
            .spawn()?,
    );
    let deadline = Instant::now() + timeout;
    let result = (|| -> Result<ExitStatus> {
        loop {
            ensure!(
                stdout.metadata()?.len() <= 1024 * 1024 && stderr.metadata()?.len() <= 1024 * 1024,
                "capture command output bound"
            );
            if let Some(status) = child.0.try_wait()? {
                return Ok(status);
            }
            ensure!(Instant::now() < deadline, "capture command deadline");
            std::thread::sleep(Duration::from_millis(10));
        }
    })();
    let status = match result {
        Ok(status) => status,
        Err(error) => {
            child.0.kill()?;
            child.wait(Duration::from_secs(5))?;
            return Err(error);
        }
    };
    let read = |file: &mut fs::File| -> Result<Vec<u8>> {
        file.seek(SeekFrom::Start(0))?;
        let mut bytes = Vec::new();
        file.take(1024 * 1024 + 1).read_to_end(&mut bytes)?;
        ensure!(bytes.len() <= 1024 * 1024, "capture command output bound");
        Ok(bytes)
    };
    Ok(Output {
        status,
        stdout: read(&mut stdout)?,
        stderr: read(&mut stderr)?,
    })
}
pub fn rpc(path: &Path, value: Value) -> Result<Value> {
    let reply = raw_rpc(path, value)?;
    ensure!(
        reply["status"] == "completed",
        "fux request failed: {reply}"
    );
    Ok(reply["result"]["value"].clone())
}
pub fn raw_rpc(path: &Path, value: Value) -> Result<Value> {
    use nix::{
        fcntl::{FcntlArg, FdFlag, OFlag, fcntl},
        sys::socket::{AddressFamily, SockFlag, SockType, UnixAddr, sockopt},
    };
    let deadline = Instant::now() + Duration::from_secs(2);
    let fd = nix::sys::socket::socket(
        AddressFamily::Unix,
        SockType::Stream,
        SockFlag::empty(),
        None,
    )?;
    fcntl(&fd, FcntlArg::F_SETFD(FdFlag::FD_CLOEXEC))?;
    fcntl(&fd, FcntlArg::F_SETFL(OFlag::O_NONBLOCK))?;
    let poll = |fd: std::os::fd::BorrowedFd<'_>, events: nix::poll::PollFlags| -> Result<()> {
        loop {
            let left = deadline
                .checked_duration_since(Instant::now())
                .context("fux request deadline")?;
            let mut polls = [nix::poll::PollFd::new(fd, events)];
            match nix::poll::poll(&mut polls, u16::try_from(left.as_millis().clamp(1, 2000))?) {
                Ok(0) | Err(nix::errno::Errno::EINTR) => continue,
                Ok(_) => return Ok(()),
                Err(e) => return Err(e.into()),
            }
        }
    };
    match nix::sys::socket::connect(fd.as_raw_fd(), &UnixAddr::new(path)?) {
        Ok(()) => {}
        Err(nix::errno::Errno::EINPROGRESS) => {
            poll(fd.as_fd(), nix::poll::PollFlags::POLLOUT)?;
            ensure!(
                nix::sys::socket::getsockopt(&fd, sockopt::SocketError)? == 0,
                "fux connect failed"
            );
        }
        Err(e) => return Err(e.into()),
    }
    let mut peer = UnixStream::from(fd);
    let write = |peer: &mut UnixStream, mut bytes: &[u8]| -> Result<()> {
        while !bytes.is_empty() {
            poll(peer.as_fd(), nix::poll::PollFlags::POLLOUT)?;
            match peer.write(bytes) {
                Ok(0) => anyhow::bail!("fux write EOF"),
                Ok(n) => bytes = &bytes[n..],
                Err(e)
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                    ) => {}
                Err(e) => return Err(e.into()),
            }
        }
        Ok(())
    };
    write(&mut peer, b"FUXCTL3\n")?;
    let mut preface = Vec::new();
    while preface.len() < 8 {
        poll(peer.as_fd(), nix::poll::PollFlags::POLLIN)?;
        let mut bytes = [0; 8];
        match peer.read(&mut bytes[..8 - preface.len()]) {
            Ok(0) => anyhow::bail!("fux preface EOF"),
            Ok(n) => preface.extend_from_slice(&bytes[..n]),
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                ) => {}
            Err(e) => return Err(e.into()),
        }
    }
    ensure!(preface == b"FUXCTL3\n", "incompatible fux");
    let mut bytes = serde_json::to_vec(&value)?;
    bytes.push(b'\n');
    write(&mut peer, &bytes)?;
    let mut reply = Vec::new();
    loop {
        poll(peer.as_fd(), nix::poll::PollFlags::POLLIN)?;
        let mut bytes = [0; 8192];
        let count = match peer.read(&mut bytes) {
            Ok(0) => anyhow::bail!("fux reply EOF"),
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
        reply.extend_from_slice(&bytes[..count]);
        ensure!(reply.len() <= 1024 * 1024, "fux reply bound");
        if let Some(end) = reply.iter().position(|b| *b == b'\n') {
            let reply: Value = serde_json::from_slice(&reply[..end])?;
            return Ok(reply);
        }
    }
}
