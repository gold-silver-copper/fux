use crate::{
    Result, ensure,
    trace::{Config, Journal},
};
use nix::{
    fcntl::{FcntlArg, OFlag, fcntl},
    sys::signal::{Signal, kill},
    unistd::Pid,
};
use portable_pty::{CommandBuilder, MasterPty, PtySize, native_pty_system};
use serde_json::{Value, json};
use std::{
    collections::{BTreeSet, VecDeque},
    fs::{self, File},
    io::{Read, Write},
    net::TcpListener,
    os::{
        fd::{AsFd, BorrowedFd},
        unix::fs::PermissionsExt,
    },
    path::{Path, PathBuf},
    process::{Child, ChildStderr, ChildStdout, Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

pub const OP: Duration = Duration::from_secs(5);
const CAP: usize = 64 * 1024;

pub struct Budget {
    pub end: Instant,
    pub interrupted: Arc<AtomicBool>,
}
impl Budget {
    pub fn check(&self, end: Instant) -> Result<()> {
        ensure(!self.interrupted.load(Ordering::Relaxed), "interrupted")?;
        ensure(
            Instant::now() < self.end.min(end),
            "observation deadline exceeded",
        )
    }
}

#[derive(Default)]
pub struct Capture {
    bytes: VecDeque<u8>,
    pub total: u64,
}
impl Capture {
    pub fn append(&mut self, data: &[u8]) {
        self.total += data.len() as u64;
        for byte in data {
            if self.bytes.len() == CAP {
                self.bytes.pop_front();
            }
            self.bytes.push_back(*byte);
        }
    }
    pub fn bytes(&self) -> Vec<u8> {
        self.bytes.iter().copied().collect()
    }
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.bytes()).into_owned()
    }
}
fn nonblocking(fd: impl AsFd) -> Result<()> {
    let flags = OFlag::from_bits_truncate(fcntl(&fd, FcntlArg::F_GETFL)?);
    fcntl(&fd, FcntlArg::F_SETFL(flags | OFlag::O_NONBLOCK))?;
    Ok(())
}
fn drain(
    reader: &mut impl Read,
    capture: &mut Capture,
    mut consume: impl FnMut(&[u8]),
) -> Result<()> {
    let mut chunk = [0; 8192];
    for _ in 0..8 {
        // never let a noisy child starve deadlines or other viewers
        match reader.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => {
                let bytes = chunk.get(..n).ok_or("invalid read length")?;
                capture.append(bytes);
                consume(bytes);
            }
            Err(e)
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.raw_os_error() == Some(nix::libc::EIO) =>
            {
                break;
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e.into()),
        }
    }
    Ok(())
}

pub struct Frontend {
    child: Box<dyn portable_pty::Child + Send + Sync>,
    master: Box<dyn MasterPty + Send>,
    io: File,
    pub capture: Capture,
    pub parser: vt100::Parser,
    pub paused: bool,
    pub exited: bool,
    pub expected_exit: bool,
    pub exit_success: bool,
    stop_attempted: bool,
    pub viewer: u64,
    original_termios: nix::sys::termios::Termios,
}
impl Frontend {
    fn spawn(
        binary: &Path,
        directory: &Path,
        endpoint: &str,
        rows: u16,
        cols: u16,
    ) -> Result<Self> {
        let pair = native_pty_system().openpty(size(rows, cols))?;
        // The master owns this FD for the duration of dup. The duplicate is owned
        // by File, and all subsequent I/O is safe, nonblocking, and single-threaded.
        let fd = pair
            .master
            .as_raw_fd()
            .ok_or("PTY has no Unix descriptor")?;
        let borrowed = unsafe { BorrowedFd::borrow_raw(fd) };
        let io = File::from(nix::unistd::dup(borrowed)?);
        nonblocking(&io)?;
        let original_termios = nix::sys::termios::tcgetattr(&io)?;
        let mut command = CommandBuilder::new(binary);
        command.arg("attach");
        command.cwd(directory);
        command.env_clear();
        command.env("PATH", "/usr/bin:/bin");
        command.env("HOME", directory);
        command.env("TERM", "xterm-256color");
        command.env("FUX_ENDPOINT", endpoint);
        let child = pair.slave.spawn_command(command)?;
        drop(pair.slave);
        Ok(Self {
            child,
            master: pair.master,
            io,
            capture: Capture::default(),
            // Pinned vt100 underflows on wrapping into a one-cell backing grid.
            // This diagnostic emulator is not the actual outer PTY: keep its
            // backing >=2x2 while still sending the exact tiny ioctl to fux.
            parser: vt100::Parser::new(rows.max(2), cols.max(2), 0),
            paused: false,
            exited: false,
            expected_exit: false,
            exit_success: false,
            stop_attempted: false,
            viewer: 0,
            original_termios,
        })
    }
    fn pump(&mut self) -> Result<()> {
        if !self.paused {
            drain(&mut self.io, &mut self.capture, |bytes| {
                // Teardown must not re-enter a decoder that just panicked.
                if !self.expected_exit {
                    self.parser.process(bytes);
                }
            })?;
        }
        if !self.exited
            && let Some(status) = self.child.try_wait()?
        {
            self.exited = true;
            self.exit_success = status.success();
        }
        Ok(())
    }
    pub fn terminal_restored(&self) -> Result<bool> {
        Ok(nix::sys::termios::tcgetattr(&self.io)? == self.original_termios)
    }
    pub fn resize(&mut self, rows: u16, cols: u16) -> Result<()> {
        ensure(!self.exited, "frontend exited before resize")?;
        self.parser.screen_mut().set_size(rows.max(2), cols.max(2));
        self.master.resize(size(rows, cols))?;
        Ok(())
    }
    pub fn send(&mut self, bytes: &[u8], budget: &Budget) -> Result<()> {
        let end = Instant::now() + OP;
        let mut remaining = bytes;
        while !remaining.is_empty() {
            budget.check(end)?;
            self.pump()?;
            ensure(!self.exited, "frontend exited before input")?;
            match self.io.write(remaining) {
                Ok(0) => return Err("PTY write returned zero".into()),
                Ok(n) => remaining = remaining.get(n..).ok_or("invalid write length")?,
                Err(e)
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                    ) =>
                {
                    thread::sleep(Duration::from_millis(2))
                }
                Err(e) => return Err(e.into()),
            }
        }
        Ok(())
    }
    /// Delivers a termination signal to the frontend process itself, as a
    /// terminal emulator or the user would, and expects a graceful exit.
    pub fn signal(&mut self, signal: Signal) -> Result<()> {
        ensure(!self.exited, "frontend exited before signal")?;
        let pid = self.child.process_id().ok_or("frontend has no PID")?;
        self.expected_exit = true;
        kill(Pid::from_raw(i32::try_from(pid)?), signal)?;
        Ok(())
    }
    pub fn stop(&mut self) -> Result<()> {
        if self.stop_attempted {
            return ensure(self.exited, "frontend previous reap attempt failed");
        }
        self.stop_attempted = true;
        self.expected_exit = true;
        if !self.exited && self.child.try_wait()?.is_none() {
            self.child.kill()?;
        }
        let end = Instant::now() + OP;
        self.paused = false;
        while !self.exited {
            self.pump()?;
            ensure(
                Instant::now() < end,
                "frontend could not be reaped after kill",
            )?;
            thread::sleep(Duration::from_millis(2));
        }
        Ok(())
    }
}
impl Drop for Frontend {
    fn drop(&mut self) {
        if !self.stop_attempted {
            let _ = self.stop();
        }
    }
}
fn size(rows: u16, cols: u16) -> PtySize {
    PtySize {
        rows,
        cols,
        pixel_width: 0,
        pixel_height: 0,
    }
}

pub struct Server {
    child: Child,
    stdout: ChildStdout,
    stderr: ChildStderr,
    out: Capture,
    err: Capture,
    pub directory: PathBuf,
    endpoint: String,
    endpoint_verified: bool,
    binary: PathBuf,
    pub frontends: Vec<Frontend>,
    pub journal: Journal,
    pub budget: Arc<Budget>,
    stopped: bool,
    cleanup_attempted: bool,
    observed_children: BTreeSet<i32>,
    /// Per-request HTTP timeout; scale scenarios raise it and record times.
    pub request_timeout: Duration,
    /// Skip journaling successful requests and responses; long walks record
    /// their own compact summary per step and keep failures verbose.
    pub quiet: bool,
    /// One connection-pooling agent per server: a fresh connection per
    /// request exhausts ephemeral ports within a few thousand requests.
    agent: Option<(Duration, ureq::Agent)>,
}
impl Server {
    pub fn spawn(
        binary: &Path,
        directory: &Path,
        config: Config,
        budget: Arc<Budget>,
    ) -> Result<Self> {
        fs::create_dir(directory)?;
        let mut journal = Journal::new(&directory.join("events.jsonl"))?;
        // Executable wrappers distinguish defaults from configured argv/output.
        // They exec, so the tracked PID remains the actual shell, not a wrapper.
        let default = directory.join("default-shell");
        fs::write(
            &default,
            "#!/bin/sh\necho $$ > initial.pid\nprintf 'DEFAULT-SHELL\\n'\nexec /bin/bash --noprofile --norc -i\n",
        )?;
        fs::set_permissions(&default, fs::Permissions::from_mode(0o700))?;
        let configured = directory.join("configured-shell");
        fs::write(
            &configured,
            "#!/bin/sh\necho $$ > initial.pid\nprintf 'CONFIGURED-SHELL\\n'\nexec /bin/bash --noprofile --norc -i\n",
        )?;
        fs::set_permissions(&configured, fs::Permissions::from_mode(0o700))?;
        match config {
            Config::Valid => fs::write(
                directory.join("fux.json"),
                serde_json::to_vec(&json!({"shell":[configured]}))?,
            )?,
            Config::Malformed => fs::write(directory.join("fux.json"), "{ invalid configuration")?,
            Config::Clipboard => fs::write(
                directory.join("fux.json"),
                serde_json::to_vec(&json!({"clipboard":"write-only"}))?,
            )?,
            Config::Missing => (),
        }
        let listener = TcpListener::bind(("127.0.0.1", 0))?;
        let port = listener.local_addr()?.port();
        journal.record("spawn_server", json!({"binary":binary,"port":port,"config":config,"SHELL":default,
            "PATH":"/usr/bin:/bin","HOME":directory,"TERM":"xterm-256color","PS1":"$ ","environment":"cleared"}))?;
        drop(listener); // inherently racy: classify bind failures, never retry a scenario
        let mut child = Command::new(binary)
            .args([
                "server",
                "--address",
                "127.0.0.1",
                "--port",
                &port.to_string(),
                "--config",
            ])
            .arg(directory.join("fux.json"))
            .current_dir(directory)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", directory)
            .env("SHELL", default)
            .env("TERM", "xterm-256color")
            .env("PS1", "$ ")
            .env("HISTFILE", "/dev/null")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let stdout = child.stdout.take().ok_or("no stdout pipe")?;
        let stderr = child.stderr.take().ok_or("no stderr pipe")?;
        // Pipe setup errors must not strand the just-spawned child.
        if let Err(error) = nonblocking(&stdout).and_then(|_| nonblocking(&stderr)) {
            let _ = child.kill();
            let end = Instant::now() + OP;
            while Instant::now() < end && matches!(child.try_wait(), Ok(None)) {
                thread::sleep(Duration::from_millis(5));
            }
            return Err(error);
        }
        Ok(Self {
            child,
            stdout,
            stderr,
            out: Capture::default(),
            err: Capture::default(),
            directory: directory.to_owned(),
            endpoint: format!("http://127.0.0.1:{port}"),
            endpoint_verified: false,
            binary: binary.to_owned(),
            frontends: Vec::new(),
            journal,
            budget,
            stopped: false,
            cleanup_attempted: false,
            observed_children: BTreeSet::new(),
            request_timeout: Duration::from_millis(500),
            quiet: false,
            agent: None,
        })
    }
    pub fn pump(&mut self) -> Result<()> {
        drain(&mut self.stdout, &mut self.out, |_| {})?;
        drain(&mut self.stderr, &mut self.err, |_| {})?;
        for frontend in &mut self.frontends {
            frontend.pump()?;
        }
        Ok(())
    }
    pub fn healthy(&mut self) -> Result<()> {
        self.pump()?;
        ensure(
            !self.frontends.iter().any(|f| f.exited && !f.expected_exit),
            "application: premature frontend exit",
        )?;
        if let Some(status) = self.child.try_wait()? {
            self.stopped = true;
            let category = if self.err.text().contains("Address already in use") {
                "setup: port collision"
            } else {
                "application: premature server exit"
            };
            return Err(format!("{category}: {status}; {}", self.err.text()).into());
        }
        Ok(())
    }
    pub fn wait(
        &mut self,
        label: &str,
        mut condition: impl FnMut(&mut Self) -> Result<bool>,
    ) -> Result<()> {
        let end = Instant::now() + OP;
        loop {
            self.budget
                .check(end)
                .map_err(|e| format!("{label}: {e}"))?;
            self.healthy()?;
            if condition(self)? {
                return Ok(());
            }
            thread::sleep(Duration::from_millis(5));
        }
    }
    fn request(&mut self, method: &str, params: Value) -> Result<Value> {
        self.budget.check(self.budget.end)?;
        let timeout = self
            .request_timeout
            .min(self.budget.end.saturating_duration_since(Instant::now()))
            .max(Duration::from_millis(50));
        if self.agent.as_ref().is_none_or(|(t, _)| *t != timeout) {
            let agent: ureq::Agent = ureq::Agent::config_builder()
                .timeout_global(Some(timeout))
                .build()
                .into();
            self.agent = Some((timeout, agent));
        }
        let agent = &self.agent.as_ref().ok_or("agent")?.1;
        let mut response = agent
            .post(&self.endpoint)
            .send_json(json!({"jsonrpc":"2.0","id":1,"method":method,"params":params}))?;
        let mut bytes = Vec::new();
        response
            .body_mut()
            .as_reader()
            .take(1024 * 1024 + 1)
            .read_to_end(&mut bytes)?;
        ensure(bytes.len() <= 1024 * 1024, "RPC response exceeded 1 MiB")?;
        let response: Value = serde_json::from_slice(&bytes)?;
        if let Some(error) = response.get("error") {
            // An internal error from one of fux's own methods is the server
            // failing to serve, not a malformed request.
            let internal = method.starts_with("fux.")
                && error.get("code").and_then(Value::as_i64) == Some(-32603);
            let prefix = if internal { "application: " } else { "" };
            return Err(format!("{prefix}{method}: {response}").into());
        }
        response
            .get("result")
            .cloned()
            .ok_or_else(|| format!("{method}: missing result").into())
    }
    pub fn announced(&mut self) -> Result<()> {
        // This line is emitted before app.run installs signal handlers and runs
        // Startup. Observe the spawn-to-readiness window without an RPC barrier.
        self.wait("server startup announcement", |s| {
            Ok(s.err.text().contains("fux trusted BRP"))
        })
    }
    pub fn ready(&mut self) -> Result<()> {
        self.wait("server readiness", |s| {
            if s.request("rpc.discover", Value::Null).is_err() {
                return Ok(false);
            }
            // Before any mutation, distinguish our initial pane from a foreign
            // fux that won the unavoidable release-to-bind port race.
            let rows = s.request(
                "world.query",
                json!({"data":{"components":["fux::model::Launch"]}}),
            )?;
            let rows = rows
                .as_array()
                .ok_or("setup: unexpected readiness response")?;
            if rows.is_empty() {
                return Ok(false);
            }
            ensure(
                rows.iter().all(|row| {
                    row.pointer("/components/fux::model::Launch/cwd") == Some(&json!(s.directory))
                }),
                "setup: port collision (endpoint belongs to another server)",
            )?;
            s.endpoint_verified = true;
            Ok(true)
        })
    }
    pub fn rpc(&mut self, method: &str, params: Value) -> Result<Value> {
        self.healthy()?;
        if self.quiet {
            let response = self.request(method, params.clone());
            if let Err(e) = &response {
                self.journal.record(
                    "rpc_failed",
                    json!({"method":method,"params":params,"error":e.to_string()}),
                )?;
                // A connection dropped mid-request is usually the server
                // dying under it; report that, which the minimizer can chase.
                self.healthy()?;
            }
            return response;
        }
        self.journal
            .record("rpc", json!({"method":method,"params":params}))?;
        let response = match self.request(method, params) {
            Ok(response) => response,
            Err(e) => {
                self.healthy()?;
                return Err(e);
            }
        };
        // Paints and large query results dominate the journal; keep their size.
        let logged = match serde_json::to_vec(&response) {
            Ok(bytes) if bytes.len() > 4096 => json!({"truncated_bytes":bytes.len()}),
            _ => response.clone(),
        };
        self.journal.record("response", logged)?;
        Ok(response)
    }
    pub fn query(&mut self, component: &str) -> Result<Vec<Value>> {
        let rows = self
            .rpc("world.query", json!({"data":{"components":[component]}}))?
            .as_array()
            .cloned()
            .ok_or("query did not return rows")?;
        if component == "fux::model::ProcessState" {
            for row in &rows {
                if let Some(pid) = row
                    .pointer("/components/fux::model::ProcessState/status/pid")
                    .and_then(Value::as_i64)
                    .and_then(|pid| i32::try_from(pid).ok())
                {
                    ensure(
                        self.observed_children.len() < 2048
                            || self.observed_children.contains(&pid),
                        "too many observed children",
                    )?;
                    self.observed_children.insert(pid);
                }
            }
        }
        Ok(rows)
    }
    pub fn control(&mut self, viewer: u64, command: Value) -> Result<()> {
        self.rpc(
            "world.trigger_event",
            json!({"event":"fux::control::Control","value":{"viewer":viewer,"command":command}}),
        )?;
        Ok(())
    }
    pub fn relation(&mut self, viewer: u64, component: &str) -> Result<u64> {
        self.query(component)?
            .iter()
            .find(|r| r.get("entity").and_then(Value::as_u64) == Some(viewer))
            .and_then(|r| r.get("components")?.get(component)?.as_u64())
            .ok_or_else(|| format!("missing {component} for {viewer}").into())
    }
    pub fn attach(&mut self, rows: u16, cols: u16) -> Result<usize> {
        ensure(
            self.frontends.iter().filter(|f| !f.exited).count() < 3,
            "too many frontends",
        )?;
        let before: Vec<_> = self
            .query("fux::model::Viewer")?
            .iter()
            .filter_map(|r| r.get("entity").and_then(Value::as_u64))
            .collect();
        self.journal
            .record("attach_pty", json!({"rows":rows,"cols":cols}))?;
        self.frontends.push(Frontend::spawn(
            &self.binary,
            &self.directory,
            &self.endpoint,
            rows,
            cols,
        )?);
        let index = self.frontends.len() - 1;
        let mut viewer = None;
        self.wait("frontend attach", |s| {
            ensure(!s.frontend(index)?.exited, "frontend exited during attach")?;
            viewer = s
                .query("fux::model::Viewer")?
                .iter()
                .filter_map(|r| r.get("entity").and_then(Value::as_u64))
                .find(|id| !before.contains(id));
            Ok(viewer.is_some() && s.frontend(index)?.capture.total > 0)
        })?;
        self.frontend(index)?.viewer = viewer.ok_or("missing attached viewer")?;
        Ok(index)
    }
    pub fn frontend(&mut self, index: usize) -> Result<&mut Frontend> {
        self.frontends
            .get_mut(index)
            .ok_or("unknown frontend".into())
    }
    pub fn send(&mut self, index: usize, bytes: &[u8]) -> Result<()> {
        self.journal
            .record("pty_input", json!({"frontend":index,"bytes":bytes}))?;
        let budget = self.budget.clone();
        self.frontend(index)?.send(bytes, &budget)
    }
    pub fn signal_frontend(&mut self, index: usize, signal: Signal) -> Result<()> {
        self.journal.record(
            "frontend_signal",
            json!({"frontend":index,"signal":signal.as_str()}),
        )?;
        self.frontend(index)?.signal(signal)
    }
    pub fn resize(&mut self, index: usize, rows: u16, cols: u16) -> Result<()> {
        self.journal.record(
            "pty_resize",
            json!({"frontend":index,"rows":rows,"cols":cols}),
        )?;
        self.frontend(index)?.resize(rows, cols)
    }
    pub fn frame(&mut self, viewer: u64, rows: u16, cols: u16) -> Result<String> {
        let value = self.rpc("fux.frame", json!({"viewer":viewer}))?;
        let paint = value
            .get("paint")
            .and_then(Value::as_str)
            .ok_or("missing paint")?;
        let mut parser = vt100::Parser::new(rows.max(2), cols.max(2), 0);
        parser.process(paint.as_bytes());
        Ok(parser.screen().contents())
    }
    /// The server's captured stderr so far, for panic and error checks.
    pub fn stderr_text(&mut self) -> Result<String> {
        self.pump()?;
        Ok(String::from_utf8_lossy(&self.err.bytes()).into_owned())
    }
    pub fn snapshot(&mut self) -> Result<()> {
        let pumped = self.pump();
        fs::write(self.directory.join("server.stdout"), self.out.bytes())?;
        fs::write(self.directory.join("server.stderr"), self.err.bytes())?;
        for (i, frontend) in self.frontends.iter().enumerate() {
            if frontend.capture.total > 0 {
                fs::write(
                    self.directory.join(format!("frontend-{i}.ansi")),
                    frontend.capture.bytes(),
                )?;
                fs::write(
                    self.directory.join(format!("frontend-{i}.txt")),
                    frontend.parser.screen().contents(),
                )?;
            }
        }
        pumped?;
        self.journal.record("snapshot", json!({"server_pid":self.child.id(),"stdout_bytes":self.out.total,"stderr_bytes":self.err.total,
            "frontends":self.frontends.iter().map(|f| json!({"pid":f.child.process_id(),"viewer":f.viewer,"exited":f.exited,"bytes":f.capture.total})).collect::<Vec<_>>()}))
    }
    pub fn signal_shutdown(&mut self) -> Result<()> {
        for frontend in &mut self.frontends {
            frontend.expected_exit = true;
        }
        let logged = self
            .journal
            .record("server_sigterm", json!({"pid":self.child.id()}));
        if self.child.try_wait()?.is_none() {
            kill(Pid::from_raw(self.child.id() as i32), Signal::SIGTERM)?;
        }
        logged
    }
    pub fn wait_server_exit(&mut self, initializing: bool) -> Result<()> {
        let end = Instant::now() + OP;
        loop {
            self.budget.check(end)?;
            self.pump()?;
            if let Some(status) = self.child.try_wait()? {
                self.stopped = true;
                self.journal
                    .record("server_exit", json!({"status":status.to_string()}))?;
                // Only the initialization race may precede signal handlers.
                use std::os::unix::process::ExitStatusExt;
                return ensure(
                    status.success()
                        || (initializing && status.signal() == Some(nix::libc::SIGTERM)),
                    &format!("application: abnormal server shutdown: {status}"),
                );
            }
            thread::sleep(Duration::from_millis(5));
        }
    }
    pub fn cleanup(&mut self) -> Result<()> {
        self.cleanup_attempted = true;
        let mut errors = Vec::new();
        // Best-effort final observation before teardown; old observations remain
        // available if interruption or the overall deadline prevents requests.
        if self.endpoint_verified
            && !self.stopped
            && self.budget.check(self.budget.end).is_ok()
            && let Err(e) = self.query("fux::model::ProcessState")
        {
            let _ = self
                .journal
                .record("final_observation_unavailable", json!(e.to_string()));
        }
        for frontend in &mut self.frontends {
            if let Err(e) = frontend.stop() {
                errors.push(e.to_string());
            }
        }
        if !self.stopped {
            if let Err(e) = self.signal_shutdown() {
                errors.push(e.to_string());
            }
            // Cleanup is allowed its own grace after interruption/run deadline.
            let end = Instant::now() + OP;
            loop {
                if let Err(e) = self.pump() {
                    errors.push(e.to_string());
                    break;
                }
                match self.child.try_wait() {
                    Ok(Some(_)) => {
                        self.stopped = true;
                        break;
                    }
                    Ok(None) if Instant::now() < end => thread::sleep(Duration::from_millis(5)),
                    Ok(None) => {
                        errors.push("server required SIGKILL during cleanup".into());
                        break;
                    }
                    Err(e) => {
                        errors.push(e.to_string());
                        break;
                    }
                }
            }
            if !self.stopped {
                let _ = self.child.kill(); // Child owns an unreaped PID, never a cached descendant PID
                let end = Instant::now() + OP;
                while Instant::now() < end {
                    if matches!(self.child.try_wait(), Ok(Some(_))) {
                        self.stopped = true;
                        break;
                    }
                    thread::sleep(Duration::from_millis(5));
                }
                if !self.stopped {
                    errors.push("server could not be reaped".into());
                }
            }
        }
        if let Ok(text) = fs::read_to_string(self.directory.join("initial.pid"))
            && let Ok(pid) = text.trim().parse::<i32>()
        {
            self.observed_children.insert(pid);
        }
        let end = Instant::now() + OP;
        while Instant::now() < end
            && self
                .observed_children
                .iter()
                .any(|pid| alive(*pid) || group_alive(*pid))
        {
            thread::sleep(Duration::from_millis(5));
        }
        let remaining: Vec<_> = self
            .observed_children
            .iter()
            .filter(|pid| alive(**pid) || group_alive(**pid))
            .copied()
            .collect();
        if !remaining.is_empty() {
            errors.push(format!("observed child PIDs/original groups remain: {remaining:?}; not signaling potentially recycled descendant IDs"));
        }
        if let Err(e) = self.snapshot() {
            errors.push(e.to_string());
        }
        if let Err(e) = self.journal.record(
            "cleanup_children",
            json!({"observed":self.observed_children,"remaining":remaining}),
        ) {
            errors.push(e.to_string());
        }
        ensure(
            errors.is_empty(),
            &format!("cleanup: {}", errors.join("; ")),
        )
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        if !self.cleanup_attempted {
            let _ = self.cleanup();
        }
    }
}

pub fn tool_version(tool: &str, arg: &str, budget: &Budget) -> Result<String> {
    let mut child = Command::new(tool)
        .arg(arg)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let mut stdout = child.stdout.take().ok_or("missing metadata stdout")?;
    let operation = (|| {
        nonblocking(&stdout)?;
        let mut capture = Capture::default();
        let end = Instant::now() + OP;
        loop {
            budget.check(end)?;
            drain(&mut stdout, &mut capture, |_| {})?;
            ensure(capture.total <= CAP as u64, "metadata output exceeded cap")?;
            if let Some(status) = child.try_wait()? {
                drain(&mut stdout, &mut capture, |_| {})?;
                ensure(status.success(), "metadata command failed")?;
                return Ok(capture.text());
            }
            thread::sleep(Duration::from_millis(5));
        }
    })();
    // Even a metadata-tool failure must not leave an unreaped child.
    if child.try_wait()?.is_none() {
        child.kill()?;
        let end = Instant::now() + OP;
        while child.try_wait()?.is_none() {
            ensure(Instant::now() < end, "metadata child could not be reaped")?;
            thread::sleep(Duration::from_millis(5));
        }
    }
    operation
}

pub fn component(row: &Value, name: &str) -> Result<Value> {
    row.get("components")
        .and_then(|v| v.get(name))
        .cloned()
        .ok_or_else(|| format!("missing {name}: {row}").into())
}
pub fn id(value: &Value) -> Result<u64> {
    value
        .get("entity")
        .and_then(Value::as_u64)
        .ok_or("missing entity".into())
}
pub fn alive(pid: i32) -> bool {
    kill(Pid::from_raw(pid), None) != Err(nix::errno::Errno::ESRCH)
}
fn group_alive(pid: i32) -> bool {
    nix::sys::signal::killpg(Pid::from_raw(pid), None) != Err(nix::errno::Errno::ESRCH)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn capture_is_a_bounded_tail() {
        let mut capture = Capture::default();
        capture.append(&vec![b'a'; CAP * 2]);
        capture.append(b"last");
        assert_eq!(capture.bytes().len(), CAP);
        assert!(capture.bytes().ends_with(b"last"));
        assert_eq!(capture.total, (CAP * 2 + 4) as u64);
    }
    #[test]
    fn deadlines_and_interrupts_fail_without_waiting() {
        let budget = Budget {
            end: Instant::now(),
            interrupted: Arc::new(AtomicBool::new(false)),
        };
        assert!(budget.check(Instant::now() + OP).is_err());
        let budget = Budget {
            end: Instant::now() + OP,
            interrupted: Arc::new(AtomicBool::new(true)),
        };
        assert!(budget.check(budget.end).is_err());
    }
}
