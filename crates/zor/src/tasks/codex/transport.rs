//! Owned, nonblocking app-server stdio. No PTY or terminal interpretation.
use super::frames::{self, Decoder};
use crate::platform::process::Running;
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    io::{Read, Write},
    os::{fd::AsRawFd, unix::process::CommandExt},
    process::{ChildStderr, ChildStdin, ChildStdout, Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

const MAX_PENDING: usize = 128;
const MAX_DIAGNOSTICS: usize = 262_144;
const MAX_PENDING_BYTES: usize = 2 * 1024 * 1024;

pub struct Client {
    process: Running,
    input: ChildStdin,
    output: ChildStdout,
    errors: ChildStderr,
    decoder: Decoder,
    pending: VecDeque<Value>,
    pending_bytes: usize,
    diagnostics: Vec<u8>,
    failed: bool,
    cancelled: Arc<AtomicBool>,
}

impl Client {
    /// The caller selects executable/configuration and credentials. This layer
    /// owns a private process group and always supplies pipe-based stdin/stdout.
    pub fn spawn(command: Command, deadline: Instant) -> Result<Self> {
        Self::spawn_cancellable(command, deadline, Arc::new(AtomicBool::new(false)))
    }

    pub fn spawn_cancellable(
        mut command: Command,
        deadline: Instant,
        cancelled: Arc<AtomicBool>,
    ) -> Result<Self> {
        ensure!(!cancelled.load(Ordering::Relaxed), "native owner stopped");
        ensure!(Instant::now() < deadline, "native launch deadline expired");
        let child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0)
            .spawn()
            .context("launch Codex app-server")?;
        let mut process = Running::new(child);
        let input = process
            .child_mut()?
            .stdin
            .take()
            .context("native stdin missing")?;
        let output = process
            .child_mut()?
            .stdout
            .take()
            .context("native stdout missing")?;
        let errors = process
            .child_mut()?
            .stderr
            .take()
            .context("native stderr missing")?;
        for fd in [input.as_raw_fd(), output.as_raw_fd(), errors.as_raw_fd()] {
            let flags = nix::fcntl::OFlag::from_bits_truncate(nix::fcntl::fcntl(
                fd,
                nix::fcntl::FcntlArg::F_GETFL,
            )?);
            nix::fcntl::fcntl(
                fd,
                nix::fcntl::FcntlArg::F_SETFL(flags | nix::fcntl::OFlag::O_NONBLOCK),
            )?;
        }
        Ok(Self {
            process,
            input,
            output,
            errors,
            decoder: Decoder::default(),
            pending: VecDeque::new(),
            pending_bytes: 0,
            diagnostics: Vec::new(),
            failed: false,
            cancelled,
        })
    }

    pub fn diagnostics(&self) -> &[u8] {
        &self.diagnostics
    }

    pub fn shutdown(mut self, deadline: Instant) -> Result<()> {
        self.failed = true;
        self.process.stop(deadline)
    }

    fn pump(&mut self) -> Result<()> {
        // Finite work in each direction keeps deadlines responsive to floods.
        for _ in 0..8 {
            let mut bytes = [0; 8192];
            match self.errors.read(&mut bytes) {
                Ok(0) => break,
                Ok(length) => {
                    ensure!(
                        self.diagnostics.len() + length <= MAX_DIAGNOSTICS,
                        "native diagnostic limit exceeded"
                    );
                    self.diagnostics
                        .extend_from_slice(bytes.get(..length).context("native stderr read")?);
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(error.into()),
            }
        }
        for _ in 0..8 {
            let mut bytes = [0; 8192];
            match self.output.read(&mut bytes) {
                Ok(0) => {
                    self.decoder.finish()?;
                    anyhow::bail!(
                        "native app-server closed its output; reconcile retained intent before any further input"
                    );
                }
                Ok(length) => {
                    let frames = self
                        .decoder
                        .push(bytes.get(..length).context("native stdout read")?)?;
                    ensure!(
                        self.pending.len() + frames.len() <= MAX_PENDING,
                        "native event queue limit exceeded"
                    );
                    for frame in frames {
                        self.pending_bytes += serde_json::to_vec(&frame)?.len();
                        ensure!(
                            self.pending_bytes <= MAX_PENDING_BYTES,
                            "native event byte limit exceeded"
                        );
                        self.pending.push_back(frame);
                    }
                    // Yield complete events before observing EOF on a later read.
                    if !self.pending.is_empty() {
                        break;
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(error.into()),
            }
        }
        Ok(())
    }

    /// Never retries a request after a failed or partial write. Even a timeout
    /// before observed acceptance consumes the connection's submission authority.
    pub fn send(&mut self, frame: &Value, deadline: Instant) -> Result<()> {
        ensure!(
            !self.failed,
            "native connection invalidated; no replay authorized"
        );
        let bytes = frames::encode(frame)?;
        let result = (|| -> Result<()> {
            ensure!(
                !self.cancelled.load(Ordering::Relaxed),
                "native owner stopped"
            );
            let mut offset = 0;
            while offset < bytes.len() {
                ensure!(
                    !self.cancelled.load(Ordering::Relaxed),
                    "native owner stopped"
                );
                ensure!(
                    Instant::now() < deadline,
                    "native write deadline exceeded; acceptance uncertain"
                );
                match self
                    .input
                    .write(bytes.get(offset..).context("native write offset")?)
                {
                    Ok(0) => anyhow::bail!("native request pipe closed"),
                    Ok(length) => offset += length,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        self.pump()?;
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(error) => return Err(error.into()),
                }
            }
            Ok(())
        })();
        if result.is_err() {
            self.failed = true;
        }
        result
    }

    /// Perform bounded nonblocking observation for an owner that also services
    /// local requests. Idle pipes are not a timeout or loss of authority.
    pub fn poll(&mut self) -> Result<Option<Value>> {
        ensure!(!self.failed, "native connection invalidated");
        let result = (|| -> Result<Option<Value>> {
            ensure!(
                !self.cancelled.load(Ordering::Relaxed),
                "native owner stopped"
            );
            if self.pending.is_empty() {
                self.pump()?;
            }
            let frame = self.pending.pop_front();
            if let Some(frame) = &frame {
                self.pending_bytes = self
                    .pending_bytes
                    .saturating_sub(serde_json::to_vec(frame)?.len());
            }
            Ok(frame)
        })();
        if result.is_err() {
            self.failed = true;
        }
        result
    }

    pub fn next(&mut self, deadline: Instant) -> Result<Value> {
        ensure!(!self.failed, "native connection invalidated");
        let result = (|| -> Result<Value> {
            loop {
                ensure!(
                    Instant::now() < deadline,
                    "native observation deadline exceeded"
                );
                if let Some(frame) = self.poll()? {
                    return Ok(frame);
                }
                std::thread::sleep(Duration::from_millis(5));
            }
        })();
        if result.is_err() {
            self.failed = true;
        }
        result
    }

    pub fn initialize(&mut self, deadline: Instant) -> Result<Value> {
        let result = (|| -> Result<Value> {
            self.send(
                &json!({"id":"zor-initialize","method":"initialize","params":{
            "clientInfo":{"name":"zor","version":env!("CARGO_PKG_VERSION")},
            "capabilities":{"experimentalApi":false}}}),
                deadline,
            )?;
            // Before initialized there is no owned thread or turn whose events can
            // be discarded. Unexpected frames fail the handshake instead.
            let reply = self.next(deadline)?;
            ensure!(
                reply.get("id").and_then(Value::as_str) == Some("zor-initialize")
                    && reply.get("error").is_none()
                    && reply.get("result").is_some(),
                "native initialization identity/result mismatch"
            );
            self.send(&json!({"method":"initialized","params":{}}), deadline)?;
            Ok(reply)
        })();
        if result.is_err() {
            self.failed = true;
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shell(script: &str) -> Command {
        let mut command = Command::new("/bin/sh");
        command.args(["-c", script]);
        command
    }

    #[test]
    fn actual_owned_pipes_preserve_literal_json_and_cleanup_live_children() -> Result<()> {
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut client = Client::spawn(
            shell("while IFS= read -r line; do printf '%s\\n' \"$line\"; done"),
            deadline,
        )?;
        let literal = json!({"text":"\n\t$(must not execute)\\é","id":"literal"});
        client.send(&literal, deadline)?;
        assert_eq!(client.next(deadline)?, literal);
        client.shutdown(deadline)?;
        Ok(())
    }

    #[test]
    fn idle_poll_preserves_authority_and_complete_frames_precede_eof() -> Result<()> {
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut client = Client::spawn(
            shell("IFS= read -r line; printf '%s\\n%s\\n' \"$line\" '{\"id\":\"last\"}'"),
            deadline,
        )?;
        for _ in 0..10 {
            assert!(client.poll()?.is_none());
        }
        let request = json!({"id":"after-idle"});
        client.send(&request, deadline)?;
        assert_eq!(client.next(deadline)?, request);
        assert_eq!(client.next(deadline)?, json!({"id":"last"}));
        assert!(client.next(deadline).is_err());
        assert!(client.poll().is_err());
        assert!(client.send(&request, deadline).is_err());
        Ok(())
    }

    #[test]
    fn owner_cancellation_interrupts_a_native_wait_and_prevents_input() -> Result<()> {
        let cancelled = Arc::new(AtomicBool::new(false));
        let mut client = Client::spawn_cancellable(
            shell("sleep 30 & wait"),
            Instant::now() + Duration::from_secs(3),
            cancelled.clone(),
        )?;
        let notifier = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(30));
            cancelled.store(true, Ordering::Relaxed);
        });
        let started = Instant::now();
        assert!(client.next(started + Duration::from_secs(30)).is_err());
        assert!(started.elapsed() < Duration::from_secs(1));
        notifier
            .join()
            .map_err(|_| anyhow::anyhow!("cancellation notifier failed"))?;
        assert!(
            client
                .send(
                    &json!({"id":"forbidden"}),
                    Instant::now() + Duration::from_secs(1)
                )
                .is_err()
        );
        client.shutdown(Instant::now() + Duration::from_secs(1))?;
        Ok(())
    }

    #[test]
    fn actual_stream_eof_and_deadlines_invalidate_the_connection() -> Result<()> {
        for script in ["printf '{\"id\":'", "sleep 30 & wait"] {
            let deadline = Instant::now() + Duration::from_millis(100);
            let mut client = Client::spawn(shell(script), deadline)?;
            assert!(client.next(deadline).is_err());
            assert!(
                client
                    .send(
                        &json!({"method":"must-not-replay"}),
                        Instant::now() + Duration::from_secs(1)
                    )
                    .is_err()
            );
            // Drop uses the existing group owner even after partial EOF or error.
        }
        Ok(())
    }

    #[test]
    fn actual_stderr_flood_is_bounded_and_prevents_further_requests() -> Result<()> {
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut client = Client::spawn(
            shell("while :; do printf 'diagnostic diagnostic diagnostic\\n' >&2; done"),
            deadline,
        )?;
        assert!(client.next(deadline).is_err());
        assert!(client.diagnostics().len() <= MAX_DIAGNOSTICS);
        assert!(
            client
                .send(&json!({"method":"must-not-replay"}), deadline)
                .is_err()
        );
        drop(client);
        Ok(())
    }

    #[test]
    fn rejected_handshakes_invalidate_the_actual_connection() -> Result<()> {
        for reply in [
            json!({"id":"wrong","result":{}}),
            json!({"id":"zor-initialize","error":{"code":1}}),
            json!({"id":"zor-initialize"}),
        ] {
            let deadline = Instant::now() + Duration::from_secs(3);
            let mut command = shell("IFS= read -r line; printf '%s\\n' \"$1\"; sleep 30 & wait");
            command.arg("fixture").arg(serde_json::to_string(&reply)?);
            let mut client = Client::spawn(command, deadline)?;
            assert!(client.initialize(deadline).is_err());
            assert!(
                client
                    .send(&json!({"method":"must-not-send"}), deadline)
                    .is_err()
            );
            client.shutdown(deadline)?;
        }
        Ok(())
    }

    #[test]
    fn partial_native_write_timeout_cannot_be_retried() -> Result<()> {
        let root = tempfile::tempdir()?;
        let marker = root.path().join("prefix");
        let mut command = shell("dd bs=1 count=16 of=\"$1\" 2>/dev/null; sleep 30 & wait");
        command.arg("fixture").arg(&marker);
        let mut client = Client::spawn(command, Instant::now() + Duration::from_secs(3))?;
        let frame = json!({"text":"x".repeat(262_144)});
        assert!(
            client
                .send(&frame, Instant::now() + Duration::from_millis(100))
                .is_err()
        );
        assert_eq!(std::fs::metadata(marker)?.len(), 16);
        assert!(
            client
                .send(&frame, Instant::now() + Duration::from_secs(1))
                .is_err()
        );
        client.shutdown(Instant::now() + Duration::from_secs(3))?;
        Ok(())
    }

    #[test]
    fn expired_shutdown_deadline_returns_and_owned_child_is_eventually_reaped() -> Result<()> {
        let mut client = Client::spawn(
            shell("sleep 30 & wait"),
            Instant::now() + Duration::from_secs(3),
        )?;
        let pid = client.process.child_mut()?.id();
        let start = Instant::now();
        assert!(client.shutdown(start).is_err());
        assert!(start.elapsed() < Duration::from_millis(500));
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            match nix::sys::signal::kill(nix::unistd::Pid::from_raw(i32::try_from(pid)?), None) {
                Err(nix::errno::Errno::ESRCH) => break,
                _ => ensure!(Instant::now() < deadline, "owned child was not reaped"),
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        Ok(())
    }

    #[test]
    #[ignore = "optional installed Codex handshake probe, no model call"]
    fn installed_app_server_accepts_the_production_stdio_handshake() -> Result<()> {
        let executable = std::env::var_os("ZOR_CODEX_PROBE_BIN")
            .context("set ZOR_CODEX_PROBE_BIN explicitly")?;
        let root = tempfile::tempdir()?;
        let mut command = Command::new(executable);
        command
            .args(["app-server", "--stdio", "-c", "mcp_servers={}"])
            .current_dir(root.path());
        let deadline = Instant::now() + Duration::from_secs(20);
        let mut client = Client::spawn(command, deadline)?;
        let response = client.initialize(deadline)?;
        ensure!(
            response.get("result").is_some(),
            "installed handshake has no result"
        );
        client.shutdown(Instant::now() + Duration::from_secs(3))?;
        Ok(())
    }
}
