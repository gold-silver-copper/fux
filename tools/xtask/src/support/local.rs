//! Public local socket clients and isolated roots for real-process scenarios.
use super::process::{OwnedProcess, wait};
use anyhow::{Context, Result, ensure};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    fs,
    io::{Read, Write},
    os::{
        fd::{AsFd, AsRawFd},
        unix::{fs::PermissionsExt, net::UnixStream},
    },
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

pub fn until<T>(timeout: Duration, mut check: impl FnMut() -> Result<Option<T>>) -> Result<T> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(value) = check()? {
            return Ok(value);
        }
        ensure!(Instant::now() < deadline, "fixture observation deadline");
        std::thread::sleep(Duration::from_millis(20));
    }
}
fn remaining(deadline: Instant) -> Result<Duration> {
    deadline
        .checked_duration_since(Instant::now())
        .filter(|left| !left.is_zero())
        .context("local request deadline")
}
pub fn connect(path: &Path, deadline: Instant) -> Result<UnixStream> {
    use nix::{
        fcntl::{FcntlArg, FdFlag, OFlag, fcntl},
        sys::socket::{AddressFamily, SockFlag, SockType, UnixAddr, sockopt},
    };
    remaining(deadline)?;
    let fd = nix::sys::socket::socket(
        AddressFamily::Unix,
        SockType::Stream,
        SockFlag::empty(),
        None,
    )?;
    fcntl(&fd, FcntlArg::F_SETFD(FdFlag::FD_CLOEXEC))?;
    fcntl(&fd, FcntlArg::F_SETFL(OFlag::O_NONBLOCK))?;
    match nix::sys::socket::connect(fd.as_raw_fd(), &UnixAddr::new(path)?) {
        Ok(()) => {}
        Err(nix::errno::Errno::EINPROGRESS) => loop {
            let timeout = u16::try_from(remaining(deadline)?.as_millis().clamp(1, 2000))?;
            let mut polls = [nix::poll::PollFd::new(
                fd.as_fd(),
                nix::poll::PollFlags::POLLOUT,
            )];
            match nix::poll::poll(&mut polls, timeout) {
                Ok(0) | Err(nix::errno::Errno::EINTR) => continue,
                Ok(_) => {}
                Err(error) => return Err(error.into()),
            }
            let error = nix::sys::socket::getsockopt(&fd, sockopt::SocketError)?;
            ensure!(
                error == 0,
                "local connect failed: {}",
                std::io::Error::from_raw_os_error(error)
            );
            break;
        },
        Err(error) => return Err(error.into()),
    }
    let stream = UnixStream::from(fd);
    stream.set_nonblocking(false)?;
    Ok(stream)
}
pub fn rpc(path: &Path, value: Value) -> Result<Value> {
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut stream = connect(path, deadline)?;
    stream.set_read_timeout(Some(remaining(deadline)?))?;
    stream.set_write_timeout(Some(remaining(deadline)?))?;
    stream.write_all(b"FUX\n")?;
    let mut preface = [0; 4];
    stream.read_exact(&mut preface)?;
    ensure!(&preface == b"FUX\n", "control protocol mismatch");
    let mut request = serde_json::to_vec(&value)?;
    request.push(b'\n');
    stream.write_all(&request)?;
    stream.set_nonblocking(true)?;
    let mut bytes = Vec::new();
    loop {
        let timeout = u16::try_from(remaining(deadline)?.as_millis().clamp(1, 2000))?;
        let mut polls = [nix::poll::PollFd::new(
            stream.as_fd(),
            nix::poll::PollFlags::POLLIN,
        )];
        match nix::poll::poll(&mut polls, timeout) {
            Ok(0) | Err(nix::errno::Errno::EINTR) => continue,
            Ok(_) => {}
            Err(error) => return Err(error.into()),
        }
        let mut chunk = [0; 8192];
        let n = match stream.read(&mut chunk) {
            Ok(n) => n,
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::Interrupted | std::io::ErrorKind::WouldBlock
                ) =>
            {
                continue;
            }
            Err(error) => return Err(error.into()),
        };
        ensure!(n != 0, "reply EOF");
        bytes.extend_from_slice(&chunk[..n]);
        ensure!(bytes.len() <= 1024 * 1024, "reply byte limit");
        if let Some(end) = bytes.iter().position(|byte| *byte == b'\n') {
            return Ok(serde_json::from_slice(&bytes[..end])?);
        }
    }
}
pub fn completed(path: &Path, value: Value) -> Result<Value> {
    let reply = rpc(path, value)?;
    ensure!(
        reply["status"] == "completed",
        "control request failed: {reply}"
    );
    Ok(reply["result"]["value"].clone())
}

pub struct Root {
    directory: tempfile::TempDir,
    pub env: BTreeMap<String, String>,
}
impl Root {
    pub fn new(prefix: &str, argv: &[String]) -> Result<Self> {
        let directory = tempfile::Builder::new()
            .prefix(prefix)
            .permissions(fs::Permissions::from_mode(0o700))
            .tempdir_in("/tmp")?;
        let root = directory.path();
        fs::create_dir_all(root.join("config/fux"))?;
        fs::write(
            root.join("config/fux/config.toml"),
            format!(
                "default-command = {{ argv = {} }}\n",
                serde_json::to_string(argv)?
            ),
        )?;
        let mut env: BTreeMap<String, String> = [
            ("PATH", "/usr/bin:/bin:/usr/sbin:/sbin"),
            ("SHELL", "/bin/sh"),
            ("TERM", "xterm-256color"),
        ]
        .into_iter()
        .map(|(k, v)| (k.into(), v.into()))
        .collect();
        for (key, path) in [
            ("HOME", root.to_owned()),
            ("XDG_RUNTIME_DIR", root.to_owned()),
            ("XDG_CONFIG_HOME", root.join("config")),
            ("XDG_STATE_HOME", root.join("state")),
        ] {
            env.insert(
                key.into(),
                path.to_str().context("UTF-8 fixture root")?.into(),
            );
        }
        Ok(Self { directory, env })
    }
    pub fn path(&self) -> &Path {
        self.directory.path()
    }
    pub fn control(&self) -> PathBuf {
        self.path().join("fux/default.sock")
    }
    pub fn command(&self, binary: &Path) -> Command {
        let mut command = Command::new(binary);
        command.env_clear().envs(&self.env).current_dir(self.path());
        command
    }
    pub fn server(&self, binary: &Path) -> Result<Server> {
        let log = tempfile::tempfile()?;
        let child = self
            .command(binary)
            .arg("serve")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(log.try_clone()?)
            .spawn()?;
        let mut owner = Server {
            child,
            finished: false,
            log,
        };
        until(Duration::from_secs(10), || {
            ensure!(
                owner.child.try_wait()?.is_none(),
                "server exited before control socket"
            );
            Ok(self.control().exists().then_some(()))
        })?;
        Ok(owner)
    }
}
pub struct Server {
    pub child: Child,
    finished: bool,
    log: fs::File,
}
impl Server {
    pub fn diagnostic(&mut self) -> Result<String> {
        use std::io::{Seek, SeekFrom};
        self.log.seek(SeekFrom::Start(0))?;
        let mut bytes = Vec::new();
        (&mut self.log).take(65537).read_to_end(&mut bytes)?;
        ensure!(
            bytes.len() <= 65536,
            "owned server diagnostic exceeds fixture limit"
        );
        Ok(String::from_utf8_lossy(&bytes).into_owned())
    }
    pub fn finish(&mut self) -> Result<()> {
        if self.child.try_wait()?.is_none() {
            self.child.terminate()?;
        }
        let status = match wait(&mut self.child, Duration::from_secs(10)) {
            Ok(status) => status,
            Err(error) => {
                let diagnostic = self
                    .diagnostic()
                    .unwrap_or_else(|failure| format!("diagnostic unavailable: {failure}"));
                return Err(error.context(format!(
                    "server shutdown failed; bounded stderr:\n{diagnostic}"
                )));
            }
        };
        self.finished = true;
        ensure!(status.success(), "owned server failed: {status}");
        Ok(())
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        if !self.finished && self.child.try_wait().is_ok_and(|status| status.is_none()) {
            let _ = self.child.terminate();
            if wait(&mut self.child, Duration::from_secs(2)).is_err() {
                let _ = self.child.kill();
                let _ = wait(&mut self.child, Duration::from_secs(3));
            }
        }
    }
}

/// Stop only the server authenticated by a disposable runtime's live manager socket.
pub fn stop_servers(runtime: &Path) -> Result<()> {
    let manager = runtime.join("fux/manager.sock");
    let peer = match connect(&manager, Instant::now() + Duration::from_secs(3)) {
        Ok(peer) => peer,
        Err(error)
            if error
                .downcast_ref::<nix::errno::Errno>()
                .is_some_and(|code| {
                    matches!(
                        code,
                        nix::errno::Errno::ENOENT | nix::errno::Errno::ECONNREFUSED
                    )
                }) =>
        {
            return Ok(());
        }
        Err(error) => return Err(error),
    };
    #[cfg(target_os = "macos")]
    let pid = nix::sys::socket::getsockopt(&peer, nix::sys::socket::sockopt::LocalPeerPid)?;
    #[cfg(target_os = "linux")]
    let pid = {
        let credentials =
            nix::sys::socket::getsockopt(&peer, nix::sys::socket::sockopt::PeerCredentials)?;
        ensure!(
            credentials.uid() == nix::unistd::getuid().as_raw(),
            "foreign server in harness runtime"
        );
        credentials.pid()
    };
    ensure!(
        pid > 1 && pid != i32::try_from(std::process::id())?,
        "invalid manager peer PID"
    );
    nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(pid),
        nix::sys::signal::Signal::SIGTERM,
    )?;
    drop(peer);
    until(Duration::from_secs(10), || {
        Ok((!manager.exists()).then_some(()))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn isolated_root_matches_private_python_mkdtemp_permissions() -> Result<()> {
        let root = Root::new("fixture-private-rs-", &["/bin/cat".into()])?;
        assert_eq!(
            fs::metadata(root.path())?.permissions().mode() & 0o777,
            0o700
        );
        Ok(())
    }
    #[test]
    fn truncated_diagnostic_cannot_establish_negative_evidence() -> Result<()> {
        let mut log = tempfile::tempfile()?;
        log.write_all(&vec![b'x'; 65536])?;
        log.write_all(b"Passphrase")?;
        let mut server = Server {
            child: Command::new("/usr/bin/true").spawn()?,
            finished: false,
            log,
        };
        assert!(server.diagnostic().is_err());
        Ok(())
    }
}
