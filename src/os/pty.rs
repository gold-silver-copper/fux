//! Pane process ownership: PTY allocation, spawn, bounded output/input pumps, process-group
//! termination and reaping. Nothing here touches the ECS World; readers report through a bounded
//! channel of typed [`Inbound`] events and the owner loop applies them in order.
//!
//! Adapted from koh (MIT); see LICENSES/koh.txt.

use crate::ecs::Inbound;
use crate::ids::PaneId;
use portable_pty::{ChildKiller, CommandBuilder, MasterPty, PtySize, native_pty_system};
use std::io::{self, Read, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{SyncSender, TrySendError, sync_channel};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;
use tokio::sync::mpsc;

const READ_CHUNK: usize = 8192;
/// Input chunks queued for the writer thread before `write_input` reports backpressure.
const WRITE_CHANNEL_DEPTH: usize = 1024;
const MAX_PENDING_INPUT_BYTES: usize = 4 * 1024 * 1024;

/// Application and credential environment variables excluded from pane inheritance.
pub fn is_private_env_key(key: &std::ffi::OsStr) -> bool {
    let key = key.to_string_lossy();
    key.starts_with("FUX_") || key.starts_with("KOH_")
}

fn scrub_parent_env(cmd: &mut CommandBuilder, keys: impl IntoIterator<Item = std::ffi::OsString>) {
    for key in keys {
        if is_private_env_key(&key) {
            cmd.env_remove(&key);
        }
    }
}

/// A running pane process. Dropping the handle kills anything still running and lets the pump
/// threads finish; the reader thread reaps the child.
pub struct PaneProcess {
    master: Option<Box<dyn MasterPty + Send>>,
    writer_tx: Option<SyncSender<InputChunk>>,
    pending_input_bytes: Arc<AtomicUsize>,
    killer: Box<dyn ChildKiller + Send + Sync>,
    pid: u32,
    reaped: Arc<AtomicBool>,
    gate: Arc<ReapGate>,
    reader: Option<std::thread::JoinHandle<()>>,
    writer: Option<std::thread::JoinHandle<()>>,
}

struct InputChunk {
    bytes: Vec<u8>,
    operation: Option<u64>,
    pending: Arc<AtomicUsize>,
}

impl Drop for InputChunk {
    fn drop(&mut self) {
        self.pending.fetch_sub(self.bytes.len(), Ordering::AcqRel);
    }
}

/// Count only bytes accepted by write; a partial failure is observable and never retried.
fn deliver_input(writer: &mut impl Write, bytes: &[u8]) -> (usize, Option<String>) {
    let mut written = 0;
    while let Some(remaining) = bytes.get(written..) {
        if remaining.is_empty() {
            break;
        }
        match writer.write(remaining) {
            Ok(0) => return (written, Some("PTY input write returned zero".into())),
            Ok(count) if count <= remaining.len() => written += count,
            Ok(_) => return (written, Some("invalid PTY write count".into())),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return (written, Some(error.to_string().chars().take(256).collect())),
        }
    }
    let error = writer
        .flush()
        .err()
        .map(|error| error.to_string().chars().take(256).collect());
    (written, error)
}

/// While a termination is in progress the leader must stay un-reaped: its zombie reserves the
/// process-group id, so the SIGKILL escalation can never hit a recycled group.
#[derive(Default)]
struct ReapGate {
    holders: Mutex<usize>,
    released: Condvar,
}

impl ReapGate {
    /// Blocks while a reap attempt is in progress, then keeps the leader un-reaped.
    fn hold(&self) {
        *self
            .holders
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) += 1;
    }
    fn release(&self) {
        let mut holders = self
            .holders
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *holders = holders.saturating_sub(1);
        if *holders == 0 {
            self.released.notify_all();
        }
    }
    /// Runs one non-blocking reap attempt while no termination holds the gate; a termination
    /// starting meanwhile waits for it, so the two can never interleave.
    fn reap_if_released<T>(&self, reap: impl FnOnce() -> T) -> T {
        let mut holders = self
            .holders
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        while *holders > 0 {
            holders = self
                .released
                .wait(holders)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
        reap()
    }
}

impl PaneProcess {
    /// Blocking: allocates a PTY, spawns `argv` (or the given fallback) and starts the pumps.
    /// Output, EOF and the final exit status are reported for `pane` on `events` in that order.
    pub fn spawn(
        pane: PaneId,
        argv: &[String],
        cwd: Option<&Path>,
        rows: u16,
        cols: u16,
        events: mpsc::Sender<Inbound>,
    ) -> io::Result<Self> {
        let pair = native_pty_system()
            .openpty(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|error| io::Error::other(format!("opening pty: {error}")))?;
        let (program, arguments) = argv
            .split_first()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "empty pane command"))?;
        let mut command = CommandBuilder::new(program);
        command.args(arguments);
        command.env("TERM", "xterm-256color");
        if let Some(cwd) = cwd {
            command.cwd(cwd);
        }
        scrub_parent_env(&mut command, std::env::vars_os().map(|(key, _)| key));
        let mut child = pair
            .slave
            .spawn_command(command)
            .map_err(|error| io::Error::other(format!("spawning pane command: {error}")))?;
        drop(pair.slave);
        let killer = child.clone_killer();
        let Some(pid) = child.process_id() else {
            let _ = child.kill();
            let _ = child.wait();
            return Err(io::Error::other("pane process has no pid"));
        };
        let reaped = Arc::new(AtomicBool::new(false));
        let gate = Arc::new(ReapGate::default());
        let mut reader = match pair.master.try_clone_reader() {
            Ok(reader) => reader,
            Err(error) => {
                let _ = kill_group(pid, nix::sys::signal::Signal::SIGKILL);
                let _ = child.wait();
                return Err(io::Error::other(format!("pty reader: {error}")));
            }
        };
        let mut writer = match pair.master.take_writer() {
            Ok(writer) => writer,
            Err(error) => {
                let _ = kill_group(pid, nix::sys::signal::Signal::SIGKILL);
                let _ = child.wait();
                return Err(io::Error::other(format!("pty writer: {error}")));
            }
        };
        let input_events = events.clone();
        let reader_reaped = Arc::clone(&reaped);
        let reader_gate = Arc::clone(&gate);
        let reader_handle = std::thread::Builder::new()
            .name(format!("fux-pane-{}", pane.0))
            .spawn(move || {
                let mut buffer = [0_u8; READ_CHUNK];
                loop {
                    match reader.read(&mut buffer) {
                        Ok(0) => break,
                        Ok(count) => {
                            let Some(chunk) = buffer.get(..count) else {
                                break;
                            };
                            if events
                                .blocking_send(Inbound::PaneOutput {
                                    pane,
                                    bytes: chunk.to_vec(),
                                })
                                .is_err()
                            {
                                break;
                            }
                        }
                        Err(_) => break,
                    }
                }
                // No further reads are possible; retaining this descriptor can keep the
                // controlling terminal open while the child is trying to finish exiting.
                drop(reader);
                let _ = events.blocking_send(Inbound::PaneEof { pane });
                // EOF only says the slave closed; wait for the real status before reporting exit.
                // Reaping is polled under the gate (a blocking `wait` would reap the leader the
                // moment SIGHUP lands) so a termination in progress keeps the group id alive
                // until it has escalated.
                let mut interval = Duration::from_millis(5);
                let code = loop {
                    match reader_gate.reap_if_released(|| child.try_wait()) {
                        Ok(Some(status)) => break exit_code(&status),
                        Ok(None) => {
                            std::thread::sleep(interval);
                            interval = (interval * 2).min(Duration::from_millis(250));
                        }
                        Err(_) => break u32::MAX,
                    }
                };
                reader_reaped.store(true, Ordering::SeqCst);
                let _ = events.blocking_send(Inbound::PaneExited { pane, code });
            })?;
        let pending_input_bytes = Arc::new(AtomicUsize::new(0));
        let (writer_tx, writer_rx) = sync_channel::<InputChunk>(WRITE_CHANNEL_DEPTH);
        let writer_handle = std::thread::Builder::new()
            .name(format!("fux-pane-input-{}", pane.0))
            .spawn(move || {
                let mut failed = false;
                while let Ok(chunk) = writer_rx.recv() {
                    let (bytes_written, error) = if failed {
                        (0, Some("PTY input writer closed".into()))
                    } else {
                        deliver_input(&mut writer, &chunk.bytes)
                    };
                    failed |= error.is_some();
                    if let Some(operation) = chunk.operation {
                        let _ = input_events.blocking_send(Inbound::InputCompleted {
                            pane,
                            operation,
                            bytes_written,
                            error,
                        });
                    }
                    // After failure continue draining/rejecting: a sender racing the first
                    // error must still receive a completion, never a silently dropped chunk.
                }
            })?;
        Ok(Self {
            master: Some(pair.master),
            writer_tx: Some(writer_tx),
            pending_input_bytes,
            killer,
            pid,
            reaped,
            gate,
            reader: Some(reader_handle),
            writer: Some(writer_handle),
        })
    }

    #[must_use]
    pub fn pid(&self) -> u32 {
        self.pid
    }

    /// Queues input for the writer thread. Full queue means the application stopped reading.
    pub fn write_input(&self, bytes: &[u8]) -> io::Result<()> {
        self.queue_input(bytes, None)
    }

    pub fn write_tracked_input(&self, operation: u64, bytes: &[u8]) -> io::Result<()> {
        self.queue_input(bytes, Some(operation))
    }

    fn queue_input(&self, bytes: &[u8], operation: Option<u64>) -> io::Result<()> {
        let Some(sender) = &self.writer_tx else {
            return Err(io::Error::from(io::ErrorKind::BrokenPipe));
        };
        self.pending_input_bytes
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |pending| {
                pending
                    .checked_add(bytes.len())
                    .filter(|sum| *sum <= MAX_PENDING_INPUT_BYTES)
            })
            .map_err(|_| {
                io::Error::new(io::ErrorKind::WouldBlock, "pane input byte budget reached")
            })?;
        let chunk = InputChunk {
            bytes: bytes.to_vec(),
            operation,
            pending: self.pending_input_bytes.clone(),
        };
        match sender.try_send(chunk) {
            Ok(()) => Ok(()),
            Err(TrySendError::Full(_)) => Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "pane input queue is full",
            )),
            Err(TrySendError::Disconnected(_)) => Err(io::Error::from(io::ErrorKind::BrokenPipe)),
        }
    }

    pub fn resize(&self, rows: u16, cols: u16) -> io::Result<()> {
        self.master
            .as_ref()
            .ok_or_else(|| io::Error::from(io::ErrorKind::BrokenPipe))?
            .resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|error| io::Error::other(format!("resizing pty: {error}")))
    }

    #[must_use]
    pub fn reaped(&self) -> bool {
        self.reaped.load(Ordering::SeqCst)
    }

    /// A signal-safe view for the termination worker: the group id and the reaped flag.
    #[must_use]
    pub fn group(&self) -> ProcessGroup {
        ProcessGroup {
            pid: self.pid,
            reaped: Arc::clone(&self.reaped),
            gate: Arc::clone(&self.gate),
            killer: self.killer.clone_killer(),
        }
    }

    /// Releases the pane: a still-running process gets the documented SIGHUP grace before
    /// SIGKILL, then the pump threads are joined. Call from a blocking context.
    pub fn join(mut self) {
        self.writer_tx.take();
        // A termination that already escalated is reaped moments after it releases the gate.
        let settle = std::time::Instant::now() + Duration::from_millis(250);
        while !self.reaped() && std::time::Instant::now() < settle {
            std::thread::sleep(Duration::from_millis(10));
        }
        if !self.reaped() {
            self.group().terminate(RELEASE_GRACE);
        }
        // Explicit release must close the control descriptor before waiting for reaping.
        // A child exiting through its controlling terminal may need the master to close.
        self.master.take();
        // Terminate before joining input: a child that stopped reading can hold write forever.
        if let Some(writer) = self.writer.take() {
            let _ = writer.join();
        }
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

impl Drop for PaneProcess {
    fn drop(&mut self) {
        // Without an explicit join the child must still die so the reader thread sees EOF.
        if !self.reaped() {
            let _ = self.killer.kill();
            let _ = kill_group(self.pid, nix::sys::signal::Signal::SIGKILL);
        }
    }
}

/// SIGHUP grace given to a process whose pane is released while it still runs.
pub const RELEASE_GRACE: Duration = Duration::from_millis(1_000);

/// Signals a pane's process group without holding the pane handle.
pub struct ProcessGroup {
    pid: u32,
    reaped: Arc<AtomicBool>,
    gate: Arc<ReapGate>,
    killer: Box<dyn ChildKiller + Send + Sync>,
}

impl ProcessGroup {
    /// SIGHUP the group, wait `grace`, then SIGKILL whatever survived (descendants that ignore
    /// SIGHUP included). The leader stays un-reaped meanwhile so the group id cannot be recycled;
    /// an already reaped leader is never signalled.
    pub fn terminate(&mut self, grace: Duration) {
        self.gate.hold();
        if self.reaped.load(Ordering::SeqCst) {
            self.gate.release();
            return;
        }
        if kill_group(self.pid, nix::sys::signal::Signal::SIGHUP).is_err() {
            let _ = self.killer.kill();
        }
        std::thread::sleep(grace);
        let _ = kill_group(self.pid, nix::sys::signal::Signal::SIGKILL);
        self.gate.release();
    }
}

fn kill_group(pid: u32, signal: nix::sys::signal::Signal) -> io::Result<()> {
    let pid = i32::try_from(pid).map_err(|_| io::Error::other("pid out of range"))?;
    nix::sys::signal::kill(nix::unistd::Pid::from_raw(-pid), signal).map_err(io::Error::other)
}

fn exit_code(status: &portable_pty::ExitStatus) -> u32 {
    match status.signal() {
        Some(signal) => 128_u32.saturating_add(signal_number(signal)),
        None => status.exit_code(),
    }
}

fn signal_number(name: &str) -> u32 {
    let trimmed = name.trim_start_matches("SIG");
    let parsed = trimmed
        .parse::<nix::sys::signal::Signal>()
        .ok()
        .or_else(|| format!("SIG{trimmed}").parse().ok());
    parsed.map_or(0, |signal| u32::try_from(signal as i32).unwrap_or(0))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Model the resource dependency deterministically: the reaper cannot finish until the
    /// control handle closes. The timeout makes an ordering regression fail instead of hang.
    #[test]
    fn explicit_release_closes_control_handle_before_joining_reaper() {
        struct ControlHandle(std::sync::mpsc::Sender<()>);
        impl Drop for ControlHandle {
            fn drop(&mut self) {
                let _ = self.0.send(());
            }
        }
        impl MasterPty for ControlHandle {
            fn resize(&self, _: PtySize) -> anyhow::Result<()> {
                Ok(())
            }
            fn get_size(&self) -> anyhow::Result<PtySize> {
                Ok(PtySize::default())
            }
            fn try_clone_reader(&self) -> anyhow::Result<Box<dyn Read + Send>> {
                anyhow::bail!("unused test operation")
            }
            fn take_writer(&self) -> anyhow::Result<Box<dyn Write + Send>> {
                anyhow::bail!("unused test operation")
            }
            fn process_group_leader(&self) -> Option<i32> {
                None
            }
            fn as_raw_fd(&self) -> Option<i32> {
                None
            }
            fn tty_name(&self) -> Option<std::path::PathBuf> {
                None
            }
        }
        #[derive(Debug)]
        struct NoProcess;
        impl ChildKiller for NoProcess {
            fn kill(&mut self) -> io::Result<()> {
                Ok(())
            }
            fn clone_killer(&self) -> Box<dyn ChildKiller + Send + Sync> {
                Box::new(Self)
            }
        }
        let (closed, wait_closed) = std::sync::mpsc::channel();
        let reaped = Arc::new(AtomicBool::new(false));
        let reader_reaped = Arc::clone(&reaped);
        let observed_close = Arc::new(AtomicBool::new(false));
        let reader_observed = Arc::clone(&observed_close);
        let reader = std::thread::spawn(move || {
            reader_observed.store(
                wait_closed.recv_timeout(Duration::from_secs(5)).is_ok(),
                Ordering::SeqCst,
            );
            reader_reaped.store(true, Ordering::SeqCst);
        });
        let process = PaneProcess {
            master: Some(Box::new(ControlHandle(closed))),
            writer_tx: None,
            pending_input_bytes: Arc::new(AtomicUsize::new(0)),
            killer: Box::new(NoProcess),
            // Fails checked PID conversion; no OS process is ever signalled by this test.
            pid: u32::MAX,
            reaped: Arc::clone(&reaped),
            gate: Arc::new(ReapGate::default()),
            reader: Some(reader),
            writer: None,
        };
        process.join();
        assert!(reaped.load(Ordering::SeqCst));
        assert!(observed_close.load(Ordering::SeqCst));
    }

    #[test]
    fn tracked_writes_report_partial_failure_and_retry_only_interruptions() {
        struct Partial {
            step: usize,
            received: Vec<u8>,
        }
        impl Write for Partial {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                self.step += 1;
                match self.step {
                    1 => Err(io::ErrorKind::Interrupted.into()),
                    2 => {
                        self.received
                            .extend_from_slice(bytes.get(..2).unwrap_or_default());
                        Ok(2)
                    }
                    _ => Err(io::ErrorKind::BrokenPipe.into()),
                }
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let mut writer = Partial {
            step: 0,
            received: Vec::new(),
        };
        let (written, error) = deliver_input(&mut writer, b"abcd");
        assert_eq!(written, 2);
        assert!(error.is_some());
        assert_eq!(writer.received, b"ab");
        assert_eq!(writer.step, 3);
        let mut complete = Vec::new();
        assert_eq!(deliver_input(&mut complete, b"abcd"), (4, None));
        assert_eq!(complete, b"abcd");
    }

    #[test]
    fn discarded_input_releases_its_byte_budget() {
        let pending = Arc::new(AtomicUsize::new(3));
        let chunk = InputChunk {
            bytes: b"abc".to_vec(),
            operation: Some(1),
            pending: pending.clone(),
        };
        drop(chunk);
        assert_eq!(pending.load(Ordering::Acquire), 0);
    }

    #[test]
    fn private_environment_keys_are_scrubbed() {
        let mut command = CommandBuilder::new("/bin/sh");
        command.env("KOH_KEY_PASSPHRASE", "secret");
        command.env("FUX_SOCKET", "/x");
        command.env("PATH", "/bin");
        scrub_parent_env(
            &mut command,
            ["KOH_KEY_PASSPHRASE", "FUX_SOCKET", "PATH"].map(std::ffi::OsString::from),
        );
        assert!(command.get_env("KOH_KEY_PASSPHRASE").is_none());
        assert!(command.get_env("FUX_SOCKET").is_none());
        assert!(command.get_env("PATH").is_some());
    }

    #[test]
    fn signal_names_map_to_exit_codes() {
        assert_eq!(signal_number("SIGKILL"), 9);
        assert_eq!(signal_number("HUP"), 1);
        assert_eq!(signal_number("nonsense"), 0);
    }
}
