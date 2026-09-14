//! Owns only a koh gateway child and its private proxy directory, never a remote application.
use super::Binding;
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, OpenOptions},
    io::Read,
    os::{
        fd::AsRawFd,
        unix::{
            fs::{DirBuilderExt, FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt},
            process::CommandExt,
        },
    },
    path::{Path, PathBuf},
    process::{ChildStderr, ChildStdout, Command, Stdio},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

/// A preparation keeps its helper alive without owning another key unlock or endpoint.
#[derive(Clone)]
pub struct GatewayLease(Arc<Mutex<Gateway>>);
impl GatewayLease {
    pub(crate) fn new(gateway: Gateway) -> Self {
        Self(Arc::new(Mutex::new(gateway)))
    }
    pub fn client(&self) -> Result<crate::service::client::Client> {
        self.0
            .lock()
            .map_err(|_| anyhow::anyhow!("control helper lock failed"))?
            .client()
    }
    pub(crate) fn poll(&self) -> Result<Option<Status>> {
        self.0
            .lock()
            .map_err(|_| anyhow::anyhow!("control helper lock failed"))?
            .poll()
    }
    /// Bounded description of the helper's last transport evidence for user messages.
    pub fn transport_description(&self) -> String {
        self.0.lock().map_or_else(
            |_| "control helper lock failed".into(),
            |gateway| gateway.transport_description(),
        )
    }
}

/// Only the owning observation worker publishes or retires a machine's control helper.
#[derive(Clone, Default)]
pub struct SharedGateway(Arc<Mutex<Option<GatewayLease>>>);
impl SharedGateway {
    pub fn current(&self) -> Result<Option<GatewayLease>> {
        Ok(self
            .0
            .lock()
            .map_err(|_| anyhow::anyhow!("control connection lock failed"))?
            .clone())
    }
    pub(crate) fn replace(&self, next: Option<GatewayLease>) -> Result<()> {
        let previous = {
            let mut current = self
                .0
                .lock()
                .map_err(|_| anyhow::anyhow!("control connection lock failed"))?;
            std::mem::replace(&mut *current, next)
        };
        // Cleanup can wait for the owned child; never hold the publication lock while it does.
        drop(previous);
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum State {
    Ready,
    Connecting,
    Connected,
    Reconnecting,
    Unauthorized,
    SessionExpired,
    SessionEnded,
    Rejected,
    Unavailable,
    Failed,
    Closed,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Status {
    pub v: u32,
    pub sequence: u64,
    pub connection: u64,
    pub state: State,
}

/// Koh reports structured transport transitions only when its `gateway connect` accepts
/// `--status-file`. The clean published companion pin does not: readiness then comes from
/// koh's own socket announcement, and unauthorized, expired and offline peers are reported
/// as one generic transport failure rather than inferred from a local socket close.
///
/// The probe runs the help text once per executable path and never unlocks a credential.
pub fn status_reporting(binary: &Path) -> Result<bool> {
    static PROBES: Mutex<Option<std::collections::BTreeMap<PathBuf, bool>>> = Mutex::new(None);
    let mut cache = PROBES
        .lock()
        .map_err(|_| anyhow::anyhow!("koh probe cache lock failed"))?;
    let cache = cache.get_or_insert_with(Default::default);
    if let Some(known) = cache.get(binary) {
        return Ok(*known);
    }
    let supported = probe_status_reporting(binary)?;
    if cache.len() < 64 {
        cache.insert(binary.to_path_buf(), supported);
    }
    Ok(supported)
}
fn probe_status_reporting(binary: &Path) -> Result<bool> {
    let mut child = Command::new(binary)
        .args(["gateway", "connect", "--help"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("probe koh gateway capabilities")?;
    let stdout = child.stdout.take().context("koh probe stdout")?;
    let deadline = Instant::now() + Duration::from_secs(5);
    let reader = std::thread::spawn(move || -> Result<Vec<u8>> {
        let mut bytes = Vec::new();
        stdout.take(65537).read_to_end(&mut bytes)?;
        Ok(bytes)
    });
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            anyhow::bail!("koh gateway capability probe timed out");
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let help = reader
        .join()
        .map_err(|_| anyhow::anyhow!("koh probe reader failed"))??;
    ensure!(help.len() <= 65536, "koh help output limit exceeded");
    ensure!(
        status.success(),
        "koh gateway connect is unavailable in this koh executable ({status})"
    );
    Ok(help.windows(13).any(|window| window == b"--status-file"))
}

pub struct Gateway {
    child: crate::platform::process::Running,
    root: Directory,
    stdout: ChildStdout,
    stderr: ChildStderr,
    announcement: Vec<u8>,
    diagnostic: Vec<u8>,
    reporting: bool,
    latest: Option<Status>,
}
impl Gateway {
    /// Does not unlock or create credentials. Koh resolves the existing key noninteractively.
    pub fn start(binding: &Binding, binary: &Path, deadline: Instant) -> Result<Self> {
        binding.validate()?;
        ensure!(
            Instant::now() < deadline,
            "gateway startup deadline exceeded"
        );
        let reporting = status_reporting(binary)?;
        let metadata =
            fs::symlink_metadata(&binding.key_file).context("koh credential file unavailable")?;
        ensure!(
            metadata.is_file() && !metadata.file_type().is_symlink(),
            "koh key-file must be an existing regular file"
        );
        let root = Directory::new()?;
        let socket = root.path.join("proxy.sock");
        let status = root.path.join("status.json");
        let mut command = Command::new(binary);
        command
            .args(["gateway", "connect"])
            .arg(&binding.endpoint)
            .arg("--key-file")
            .arg(&binding.key_file)
            .arg("--socket")
            .arg(&socket)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0);
        if reporting {
            command.arg("--status-file").arg(status);
        }
        if let Some(direct) = binding.direct {
            command.arg("--direct").arg(direct.to_string());
        }
        if let Some(relay) = &binding.relay_url {
            command.arg("--relay-url").arg(relay);
        }
        let mut child = crate::platform::process::Running::new(
            command.spawn().context("start koh gateway helper")?,
        );
        let stdout = child
            .child_mut()?
            .stdout
            .take()
            .context("gateway announcement pipe")?;
        let stderr = child
            .child_mut()?
            .stderr
            .take()
            .context("gateway diagnostic pipe")?;
        for pipe in [stdout.as_raw_fd(), stderr.as_raw_fd()] {
            nix::fcntl::fcntl(
                pipe,
                nix::fcntl::FcntlArg::F_SETFL(nix::fcntl::OFlag::O_NONBLOCK),
            )?;
        }
        let mut gateway = Self {
            child,
            root,
            stdout,
            stderr,
            announcement: Vec::new(),
            diagnostic: Vec::new(),
            reporting,
            latest: None,
        };
        loop {
            gateway.poll()?;
            // Koh announces its private socket only after the endpoint is bound. That
            // announcement is local helper readiness, never remote admission.
            if (gateway.latest.is_some() || !reporting)
                && announced(&gateway.announcement, &socket)?
                && fs::symlink_metadata(&socket)
                    .is_ok_and(|metadata| metadata.file_type().is_socket())
            {
                return Ok(gateway);
            }
            ensure!(
                Instant::now() < deadline,
                "koh helper startup timed out; noninteractive existing credentials are required ({})",
                gateway.diagnostic()
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    /// Whether koh publishes structured transport transitions for this helper.
    pub fn reports_status(&self) -> bool {
        self.reporting
    }
    /// Bounded, non-secret description of the last known transport state for messages.
    pub fn transport_description(&self) -> String {
        match (&self.latest, self.reporting) {
            (Some(status), _) => format!("{:?}", status.state),
            (None, true) => "no koh transport report yet".into(),
            (None, false) => {
                "koh transport status unavailable (this koh lacks --status-file)".into()
            }
        }
    }
    pub fn socket(&self) -> PathBuf {
        self.root.path.join("proxy.sock")
    }
    pub fn client(&self) -> Result<crate::service::client::Client> {
        crate::service::client::Client::socket(self.socket())
    }
    /// Read at most one bounded status file and a bounded amount of diagnostics per call.
    pub fn poll(&mut self) -> Result<Option<Status>> {
        self.drain()?;
        if let Some(status) = self.child.try_wait()? {
            anyhow::bail!("koh helper exited ({status}); {}", self.diagnostic());
        }
        if !self.reporting {
            return Ok(None);
        }
        let Some(status) = read_status(&self.root.path.join("status.json"), self.latest.as_ref())?
        else {
            return Ok(None);
        };
        self.latest = Some(status.clone());
        Ok(Some(status))
    }
    pub fn latest(&self) -> Option<&Status> {
        self.latest.as_ref()
    }
    fn drain(&mut self) -> Result<()> {
        drain_pipe(
            &mut self.stderr,
            &mut self.diagnostic,
            16 * 1024,
            "diagnostic",
        )?;
        drain_pipe(
            &mut self.stdout,
            &mut self.announcement,
            4096,
            "announcement",
        )
    }
    fn diagnostic(&self) -> String {
        String::from_utf8_lossy(&self.diagnostic)
            .chars()
            .filter(|ch| !ch.is_control() || *ch == '\n')
            .take(1024)
            .collect()
    }
}
fn drain_pipe(
    pipe: &mut impl Read,
    retained: &mut Vec<u8>,
    limit: usize,
    label: &str,
) -> Result<()> {
    let mut buffer = [0u8; 2048];
    for _ in 0..8 {
        match pipe.read(&mut buffer) {
            Ok(0) => break,
            Ok(count) => {
                ensure!(
                    retained.len() + count <= limit,
                    "koh helper {label} limit exceeded"
                );
                retained.extend_from_slice(buffer.get(..count).context("pipe read count")?);
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}
/// Koh prints exactly one JSON line naming the socket it bound. Any other complete line is
/// an incompatible announcement; a partial line is not yet ready.
fn announced(announcement: &[u8], socket: &Path) -> Result<bool> {
    let Some(end) = announcement.iter().position(|byte| *byte == b'\n') else {
        return Ok(false);
    };
    let line: serde_json::Value =
        serde_json::from_slice(announcement.get(..end).context("announcement line bound")?)
            .context("incompatible koh gateway announcement")?;
    ensure!(
        line.get("socket").and_then(|value| value.as_str()) == socket.to_str(),
        "koh announced an unexpected socket"
    );
    Ok(true)
}
fn read_status(path: &Path, previous: Option<&Status>) -> Result<Option<Status>> {
    let file = match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(previous.cloned());
        }
        Err(error) => return Err(error).context("read koh status"),
    };
    let metadata = file.metadata()?;
    ensure!(
        metadata.is_file()
            && metadata.uid() == nix::unistd::geteuid().as_raw()
            && metadata.permissions().mode() & 0o077 == 0,
        "unsafe koh status file"
    );
    let mut bytes = Vec::new();
    file.take(2049).read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= 2048, "koh status byte limit");
    let status: Status =
        serde_json::from_slice(&bytes).context("incompatible koh transport status")?;
    ensure!(
        status.v == 1 && previous.is_none_or(|last| status.sequence >= last.sequence),
        "incompatible or regressed koh status"
    );
    Ok(Some(status))
}

impl Drop for Gateway {
    fn drop(&mut self) {
        // Signal only this unreaped child/group. Bounded grace permits koh to remove its socket.
        if matches!(self.child.try_wait(), Ok(None)) {
            if let Ok(child) = self.child.child_mut()
                && let Ok(pid) = i32::try_from(child.id())
            {
                let _ = nix::sys::signal::killpg(
                    nix::unistd::Pid::from_raw(pid),
                    nix::sys::signal::Signal::SIGTERM,
                );
            }
            let deadline = Instant::now() + Duration::from_millis(500);
            while Instant::now() < deadline && matches!(self.child.try_wait(), Ok(None)) {
                std::thread::sleep(Duration::from_millis(10));
            }
        }
        let _ = self.child.stop(Instant::now() + Duration::from_secs(2));
    }
}

struct Directory {
    path: PathBuf,
    dev: u64,
    ino: u64,
}
impl Directory {
    fn new() -> Result<Self> {
        // Keep Unix socket paths short on both macOS and Linux; the exclusive 0700 leaf is ours.
        let path = Path::new("/tmp").join(format!("zor-gw-{}", local_ipc::random_token()?));
        fs::DirBuilder::new().mode(0o700).create(&path)?;
        let metadata = fs::symlink_metadata(&path)?;
        Ok(Self {
            path,
            dev: metadata.dev(),
            ino: metadata.ino(),
        })
    }
}
impl Drop for Directory {
    fn drop(&mut self) {
        if !fs::symlink_metadata(&self.path)
            .is_ok_and(|m| m.is_dir() && m.dev() == self.dev && m.ino() == self.ino)
        {
            return;
        }
        // The fresh private directory is reserved to this helper. Never recurse into any entry.
        if let Ok(entries) = fs::read_dir(&self.path) {
            for entry in entries.flatten() {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                let owned_name = name == "proxy.sock"
                    || name == "status.json"
                    || (name.starts_with("status.") && name.ends_with(".tmp"));
                if owned_name
                    && fs::symlink_metadata(entry.path()).is_ok_and(|m| {
                        m.uid() == nix::unistd::geteuid().as_raw()
                            && (m.is_file() || m.file_type().is_socket())
                    })
                {
                    let _ = fs::remove_file(entry.path());
                }
            }
        }
        let _ = fs::remove_dir(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_rejects_unsafe_malformed_and_regressed_evidence() -> Result<()> {
        let root = tempfile::tempdir()?;
        let path = root.path().join("status.json");
        assert!(read_status(&path, None)?.is_none());
        let write = |bytes: &[u8]| -> Result<()> {
            fs::write(&path, bytes)?;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
            Ok(())
        };
        write(br#"{"v":1,"sequence":4,"connection":2,"state":"unauthorized"}"#)?;
        let current = read_status(&path, None)?.context("status missing")?;
        assert_eq!(current.state, State::Unauthorized);
        let newer = Status {
            sequence: 5,
            ..current.clone()
        };
        assert!(read_status(&path, Some(&newer)).is_err());
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644))?;
        assert!(read_status(&path, None).is_err());
        for bytes in [b"{}".as_slice(), b"{", &vec![b'x'; 2049]] {
            write(bytes)?;
            assert!(read_status(&path, None).is_err());
        }
        fs::remove_file(&path)?;
        std::os::unix::fs::symlink(root.path().join("absent"), &path)?;
        assert!(read_status(&path, None).is_err());
        Ok(())
    }

    #[test]
    fn retiring_a_control_connection_preserves_only_existing_leases() -> Result<()> {
        fn helper() -> Result<GatewayLease> {
            let mut child = Command::new("/bin/sleep")
                .arg("30")
                .process_group(0)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()?;
            let stdout = child.stdout.take().context("announcement pipe")?;
            let stderr = child.stderr.take().context("diagnostic pipe")?;
            Ok(GatewayLease::new(Gateway {
                child: crate::platform::process::Running::new(child),
                root: Directory::new()?,
                stdout,
                stderr,
                announcement: Vec::new(),
                diagnostic: Vec::new(),
                reporting: true,
                latest: None,
            }))
        }
        let published = SharedGateway::default();
        let first = helper()?;
        let original = first
            .0
            .lock()
            .map_err(|_| anyhow::anyhow!("helper lock"))?
            .root
            .path
            .clone();
        published.replace(Some(first.clone()))?;
        let preparation = published.current()?.context("published control")?;
        drop(first);
        published.replace(None)?;
        assert!(published.current()?.is_none());
        assert!(
            original.exists(),
            "retirement killed an in-flight preparation's helper"
        );
        let replacement = helper()?;
        let replacement_path = replacement
            .0
            .lock()
            .map_err(|_| anyhow::anyhow!("helper lock"))?
            .root
            .path
            .clone();
        published.replace(Some(replacement.clone()))?;
        assert!(!Arc::ptr_eq(&preparation.0, &replacement.0));
        assert!(Arc::ptr_eq(
            &published.current()?.context("replacement")?.0,
            &replacement.0
        ));
        drop(preparation);
        assert!(
            !original.exists(),
            "last preparation leaked its retired helper"
        );
        assert!(replacement_path.exists());
        drop(replacement);
        published.replace(None)?;
        assert!(
            !replacement_path.exists(),
            "last owner leaked the replacement helper"
        );
        Ok(())
    }

    #[test]
    fn capability_probe_reads_help_once_and_announcement_requires_the_exact_socket() -> Result<()> {
        let root = tempfile::tempdir()?;
        let script = |name: &str, body: &str| -> Result<PathBuf> {
            let path = root.path().join(name);
            fs::write(&path, format!("#!/bin/sh\n{body}\n"))?;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o700))?;
            Ok(path)
        };
        let counter = root.path().join("count");
        let modern = script(
            "modern",
            &format!(
                "echo probe >> '{}'; echo 'Options: --status-file <PATH> --socket <PATH>'",
                counter.display()
            ),
        )?;
        let published = script(
            "published",
            "echo 'Options: --socket <PATH> --key-file <PATH>'",
        )?;
        let broken = script(
            "broken",
            "echo 'error: unrecognized subcommand' >&2; exit 2",
        )?;
        assert!(status_reporting(&modern)?);
        assert!(status_reporting(&modern)?);
        assert_eq!(
            fs::read_to_string(&counter)?.lines().count(),
            1,
            "probe ran twice"
        );
        assert!(!status_reporting(&published)?);
        assert!(status_reporting(&broken).is_err());
        let socket = root.path().join("proxy.sock");
        assert!(!announced(b"", &socket)?);
        assert!(!announced(br#"{"socket":"#, &socket)?);
        assert!(announced(
            format!("{}\n", serde_json::json!({"socket": socket})).as_bytes(),
            &socket
        )?);
        assert!(announced(b"{\"socket\":\"/elsewhere\"}\n", &socket).is_err());
        assert!(announced(b"not json\n", &socket).is_err());
        Ok(())
    }

    #[test]
    fn directory_cleanup_removes_owned_entries_but_preserves_unknown_files() -> Result<()> {
        let root = Directory::new()?;
        let path = root.path.clone();
        fs::write(path.join("status.json"), b"owned")?;
        let listener = std::os::unix::net::UnixListener::bind(path.join("proxy.sock"))?;
        drop(listener);
        fs::write(path.join("user-file"), b"preserve")?;
        drop(root);
        assert!(!path.join("status.json").exists());
        assert!(!path.join("proxy.sock").exists());
        assert_eq!(fs::read(path.join("user-file"))?, b"preserve");
        fs::remove_file(path.join("user-file"))?;
        fs::remove_dir(path)?;
        let empty = Directory::new()?;
        let path = empty.path.clone();
        drop(empty);
        assert!(!path.exists());
        Ok(())
    }
}
