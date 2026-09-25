//! A real server, real `fux` CLI calls, and scripted attach clients that
//! speak the protocol and keep a fux-vt screen of what they were painted.
#![allow(
    dead_code,
    reason = "each test binary uses a different part of this module"
)]
use fux::protocol::{Decoder, Frame, PROTOCOL, Role};
use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

pub type Outcome = Result<(), String>;

pub const FUX: &str = env!("CARGO_BIN_EXE_fux");
/// How long a test waits for something to show.
pub const PATIENCE: Duration = Duration::from_secs(10);

static COUNT: AtomicUsize = AtomicUsize::new(0);
/// Servers are started one at a time, so no fork in one test can inherit
/// another test's descriptors.
static SPAWN: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// When a wait that starts now gives up. A time past what an `Instant`
/// holds gives up at once.
pub fn after(wait: Duration) -> Instant {
    let now = Instant::now();
    now.checked_add(wait).unwrap_or(now)
}

pub fn e(error: impl std::fmt::Display) -> String {
    error.to_string()
}

pub struct Server {
    pub dir: PathBuf,
    pub socket: PathBuf,
    child: Option<Child>,
}

pub struct Output {
    pub status: i32,
    pub stdout: String,
    pub stderr: String,
}

impl Server {
    /// A server whose shell is `sh` with a plain prompt, and `config` lines.
    pub fn start(config: &str) -> Result<Server, String> {
        Server::start_limited(config, None)
    }

    /// The same, with at most `open_files` descriptors (`ulimit -n`).
    pub fn start_limited(config: &str, open_files: Option<u32>) -> Result<Server, String> {
        Server::start_with(config, open_files, false).map(|(server, _)| server)
    }

    /// A server that inherits a descriptor without close-on-exec, as one
    /// does whatever its parent leaked into it: the file `inherited` in its
    /// directory, and the descriptor's number, which is the same in the
    /// server. It is made while no other server can start, so only this one
    /// inherits it.
    pub fn start_inheriting(config: &str) -> Result<(Server, i32), String> {
        let (server, held) = Server::start_with(config, None, true)?;
        Ok((server, held.ok_or("a held descriptor")?))
    }

    fn start_with(
        config: &str,
        open_files: Option<u32>,
        inherit: bool,
    ) -> Result<(Server, Option<i32>), String> {
        let base = std::env::temp_dir().canonicalize().map_err(e)?;
        let dir = base.join(format!(
            "fux-t{}-{}",
            std::process::id(),
            COUNT.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).map_err(e)?;
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).map_err(e)?;
        let config_path = dir.join("fux.conf");
        std::fs::write(&config_path, format!("set shell /bin/sh\n{config}\n")).map_err(e)?;
        let socket = dir.join("s").join("fux.sock");
        let log = std::fs::File::create(dir.join("server.log")).map_err(e)?;
        let (child, held) = {
            let _guard = SPAWN.lock().map_err(e)?;
            let leaked = if inherit {
                let file = std::fs::File::create(dir.join("inherited")).map_err(e)?;
                Some(fuxix::io::duplicate_inheritable(&file).map_err(e)?)
            } else {
                None
            };
            let mut command = match open_files {
                // `exec` keeps the pid, so `pid()` is the server's.
                Some(n) => {
                    let mut sh = Command::new("/bin/sh");
                    sh.arg("-c")
                        .arg(format!("ulimit -n {n} && exec \"$0\" \"$@\""))
                        .arg(FUX);
                    sh
                }
                None => Command::new(FUX),
            };
            let child = command
                .arg("server")
                .arg("--socket")
                .arg(&socket)
                .arg("--config")
                .arg(&config_path)
                .env("PS1", "$ ")
                .env("SHELL", "/bin/sh")
                .env("ENV", "/dev/null")
                .env("HOME", &dir)
                .env_remove("FUX_PANE")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(log)
                .spawn()
                .map_err(e)?;
            // Closed here once the server has its copy.
            let held = leaked.as_ref().map(std::os::fd::AsRawFd::as_raw_fd);
            (child, held)
        };
        let server = Server {
            dir,
            socket,
            child: Some(child),
        };
        let deadline = after(PATIENCE);
        while UnixStream::connect(&server.socket).is_err() {
            if Instant::now() > deadline {
                return Err(format!("the server did not start: {}", server.log()));
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        Ok((server, held))
    }

    /// The server's process id, while it runs.
    pub fn pid(&self) -> Option<u32> {
        self.child.as_ref().map(Child::id)
    }

    pub fn log(&self) -> String {
        std::fs::read_to_string(self.dir.join("server.log")).unwrap_or_default()
    }

    /// Runs `fux ARGS…` against this server.
    pub fn fux(&self, args: &[&str]) -> Result<Output, String> {
        self.fux_env(args, &[])
    }

    pub fn fux_env(&self, args: &[&str], env: &[(&str, &str)]) -> Result<Output, String> {
        let mut command = Command::new(FUX);
        command
            .args(args)
            .env("FUX_SOCKET", &self.socket)
            .env_remove("FUX_PANE");
        for (k, v) in env {
            command.env(k, v);
        }
        let out = command.output().map_err(e)?;
        Ok(Output {
            status: out.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        })
    }

    /// Runs a command that must succeed; its stdout.
    pub fn ok(&self, args: &[&str]) -> Result<String, String> {
        let out = self.fux(args)?;
        if out.status != 0 {
            return Err(format!(
                "fux {args:?} failed ({}): {}",
                out.status, out.stderr
            ));
        }
        Ok(out.stdout)
    }

    pub fn attach(&self, rows: u16, cols: u16) -> Result<Client, String> {
        Client::attach(&self.socket, rows, cols, None)
    }

    /// Waits until the server has exited; its exit status.
    pub fn wait_exit(&mut self) -> Result<std::process::ExitStatus, String> {
        let deadline = after(PATIENCE);
        loop {
            if let Some(child) = &mut self.child
                && let Some(status) = child.try_wait().map_err(e)?
            {
                self.child = None;
                return Ok(status);
            }
            if Instant::now() > deadline {
                return Err("the server did not exit".into());
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        // Test output is shown only for a failing test, so the server's log
        // is always printed; it explains a failure the assertion cannot.
        eprintln!(
            "--- server log ---\n{}--- end of server log ---",
            self.log()
        );
        if let Some(mut child) = self.child.take() {
            let _ = Command::new(FUX)
                .arg("kill-server")
                .env("FUX_SOCKET", &self.socket)
                .output();
            let deadline = after(Duration::from_secs(3));
            while child.try_wait().ok().flatten().is_none() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(10));
            }
            let _ = child.kill();
            let _ = child.wait();
        }
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// A scripted attach client: what it sends, and a terminal of what it was sent.
pub struct Client {
    stream: UnixStream,
    decoder: Decoder,
    pub terminal: fux_vt::Parser,
    pub rows: u16,
    pub cols: u16,
    pub exit: Option<String>,
    /// Every byte of every paint, for tests that look at the sequences.
    pub painted: Vec<u8>,
}

impl Client {
    pub fn attach(
        socket: &Path,
        rows: u16,
        cols: u16,
        workspace: Option<&str>,
    ) -> Result<Client, String> {
        let mut stream = UnixStream::connect(socket).map_err(e)?;
        let hello = Frame::Hello {
            protocol: PROTOCOL,
            version: "test".into(),
            role: Role::Attach,
        };
        stream.write_all(&hello.encode()?).map_err(e)?;
        let attach = Frame::Attach {
            rows,
            cols,
            workspace: workspace.map(str::to_owned),
        };
        stream.write_all(&attach.encode()?).map_err(e)?;
        stream.set_nonblocking(true).map_err(e)?;
        Ok(Client {
            stream,
            decoder: Decoder::default(),
            terminal: fux_vt::Parser::new(rows, cols, 0).map_err(e)?,
            rows,
            cols,
            exit: None,
            painted: Vec::new(),
        })
    }

    pub fn send(&mut self, bytes: &[u8]) -> Outcome {
        self.stream.set_nonblocking(false).map_err(e)?;
        let result = self
            .stream
            .write_all(&Frame::Input(bytes.to_vec()).encode()?)
            .map_err(e);
        self.stream.set_nonblocking(true).map_err(e)?;
        result
    }

    /// Types text, as keys.
    pub fn keys(&mut self, text: &str) -> Outcome {
        self.send(text.as_bytes())
    }

    pub fn resize(&mut self, rows: u16, cols: u16) -> Outcome {
        self.rows = rows;
        self.cols = cols;
        self.terminal = fux_vt::Parser::new(rows, cols, 0).map_err(e)?;
        self.stream.set_nonblocking(false).map_err(e)?;
        let result = self
            .stream
            .write_all(&Frame::Resize { rows, cols }.encode()?)
            .map_err(e);
        self.stream.set_nonblocking(true).map_err(e)?;
        result
    }

    pub fn detach(&mut self) -> Outcome {
        self.stream.set_nonblocking(false).map_err(e)?;
        let result = self.stream.write_all(&Frame::Detach.encode()?).map_err(e);
        self.stream.set_nonblocking(true).map_err(e)?;
        result
    }

    /// Reads whatever the server has sent.
    pub fn pump(&mut self) -> Outcome {
        let mut buffer = vec![0u8; 64 * 1024];
        loop {
            match self.stream.read(&mut buffer) {
                Ok(0) => {
                    if self.exit.is_none() {
                        self.exit = Some("closed".into());
                    }
                    break;
                }
                Ok(n) => self.decoder.push(buffer.get(..n).unwrap_or_default()),
                Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(err) => return Err(e(err)),
            }
        }
        while let Some(frame) = self.decoder.frame()? {
            match frame {
                Frame::Paint(bytes) => {
                    self.painted.extend_from_slice(&bytes);
                    self.terminal.process(&bytes).map_err(e)?;
                }
                Frame::Exit(reason) => self.exit = Some(reason),
                Frame::Hello { .. }
                | Frame::Attach { .. }
                | Frame::Input(_)
                | Frame::Resize { .. }
                | Frame::Detach
                | Frame::Command { .. }
                | Frame::Stdout(_)
                | Frame::Stderr(_)
                | Frame::Done { .. } => {}
            }
        }
        Ok(())
    }

    /// The screen's rows, as text.
    pub fn lines(&self) -> Vec<String> {
        let screen = self.terminal.screen();
        let window = screen.window(0, self.rows, self.cols);
        (0..self.rows)
            .map(|y| {
                let mut line = String::new();
                if let Some(row) = window.row(y) {
                    for cell in row.cells {
                        if cell.is_wide_continuation() {
                            continue;
                        }
                        line.push_str(if cell.has_contents() {
                            cell.contents()
                        } else {
                            " "
                        });
                    }
                }
                line.trim_end().to_owned()
            })
            .collect()
    }

    pub fn text(&self) -> String {
        self.lines().join("\n")
    }

    /// Waits until the screen satisfies `test`.
    pub fn wait(&mut self, what: &str, test: impl Fn(&str) -> bool) -> Outcome {
        let deadline = after(PATIENCE);
        loop {
            self.pump()?;
            let text = self.text();
            if test(&text) {
                return Ok(());
            }
            if Instant::now() > deadline {
                return Err(format!("waited for {what}; the screen is:\n{text}"));
            }
            std::thread::sleep(Duration::from_millis(15));
        }
    }

    pub fn wait_for(&mut self, needle: &str) -> Outcome {
        let needle = needle.to_owned();
        self.wait(&format!("{needle:?}"), move |t| t.contains(&needle))
    }

    /// Waits for the server to end this attachment; the reason.
    pub fn wait_exit(&mut self) -> Result<String, String> {
        let deadline = after(PATIENCE);
        loop {
            self.pump()?;
            if let Some(reason) = &self.exit {
                return Ok(reason.clone());
            }
            if Instant::now() > deadline {
                return Err("the attachment did not end".into());
            }
            std::thread::sleep(Duration::from_millis(15));
        }
    }

    /// The bottom row: the bar.
    pub fn bar(&self) -> String {
        self.lines().last().cloned().unwrap_or_default()
    }
}

/// The CPU time a process has used, in seconds.
pub fn cpu_seconds(pid: u32) -> Result<f64, String> {
    if let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) {
        // After the command name, which is in parentheses and may hold
        // spaces, the fields start at the state, field 3: user and system
        // time, fields 14 and 15, are the 12th and 13th here, in ticks.
        let (_, rest) = stat.rsplit_once(')').ok_or("a /proc stat line")?;
        let fields: Vec<&str> = rest.split_whitespace().collect();
        let ticks = |i: usize| -> Result<f64, String> {
            fields
                .get(i)
                .ok_or("a /proc stat field")?
                .parse::<f64>()
                .map_err(e)
        };
        let hz = Command::new("getconf").arg("CLK_TCK").output().map_err(e)?;
        let hz: f64 = String::from_utf8_lossy(&hz.stdout)
            .trim()
            .parse()
            .map_err(e)?;
        return Ok((ticks(11)? + ticks(12)?) / hz);
    }
    // macOS: `ps` gives minutes and seconds to the hundredth.
    let out = Command::new("ps")
        .args(["-o", "time=", "-p", &pid.to_string()])
        .output()
        .map_err(e)?;
    let text = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    let (minutes, seconds) = text.split_once(':').ok_or(format!("ps time {text:?}"))?;
    Ok(minutes.parse::<f64>().map_err(e)? * 60.0 + seconds.parse::<f64>().map_err(e)?)
}

/// Whether a process exists and has not exited: a zombie, which lingers
/// until its parent reaps it, counts as gone.
pub fn alive(pid: i32) -> bool {
    let Some(p) = fuxix::process::Pid::from_raw(pid) else {
        return false;
    };
    if !fuxix::process::exists(p) {
        return false;
    }
    Command::new("ps")
        .args(["-o", "stat=", "-p", &pid.to_string()])
        .output()
        .is_ok_and(|out| {
            let stat = String::from_utf8_lossy(&out.stdout);
            let stat = stat.trim();
            !stat.is_empty() && !stat.starts_with('Z')
        })
}

/// Sends a signal to a process, if it is there.
pub fn signal(pid: i32, signal: fuxix::process::Signal) {
    if let Some(p) = fuxix::process::Pid::from_raw(pid) {
        let _ = fuxix::process::kill(p, signal);
    }
}

/// Processes a test started outside fux's care, killed when it ends,
/// however it ends.
#[derive(Default)]
pub struct Reap(pub Vec<i32>);

impl Drop for Reap {
    fn drop(&mut self) {
        for pid in &self.0 {
            signal(*pid, fuxix::process::Signal::Kill);
        }
    }
}

/// Reads a pid a shell wrote into a file, waiting for it.
pub fn pid_in(path: &Path) -> Result<i32, String> {
    let mut pid = None;
    eventually(&format!("a pid in {}", path.display()), || {
        pid = std::fs::read_to_string(path)
            .ok()
            .and_then(|t| t.trim().parse().ok());
        Ok(pid.is_some())
    })?;
    pid.ok_or_else(|| "a pid".into())
}

impl Server {
    /// The panes `fux ls` lists, in order, with their shells' pids.
    pub fn panes(&self) -> Result<Vec<(String, i32)>, String> {
        Ok(self
            .ok(&["ls"])?
            .lines()
            .filter_map(|line| {
                let mut words = line.split_whitespace();
                let id = words.next().filter(|w| w.starts_with('%'))?;
                let pid = line.rsplit_once(" pid ")?.1.trim().parse().ok()?;
                Some((id.to_owned(), pid))
            })
            .collect())
    }

    /// The pane with the highest number: the one made last.
    pub fn newest_pane(&self) -> Result<(String, i32), String> {
        self.panes()?
            .into_iter()
            .max_by_key(|(id, _)| id.trim_start_matches('%').parse::<u32>().unwrap_or(0))
            .ok_or_else(|| "no pane".into())
    }

    /// Types a line into a pane once its shell shows a prompt.
    pub fn type_line(&self, pane: &str, line: &str) -> Outcome {
        eventually(&format!("a prompt in {pane}"), || {
            Ok(self.ok(&["capture-pane", "-t", pane])?.contains('$'))
        })?;
        self.ok(&["send-keys", "-t", pane, "-l", line])?;
        self.ok(&["send-keys", "-t", pane, "Enter"])?;
        Ok(())
    }
}

/// Waits until `test` holds, polling.
pub fn eventually(what: &str, mut test: impl FnMut() -> Result<bool, String>) -> Outcome {
    let deadline = after(PATIENCE);
    loop {
        if test()? {
            return Ok(());
        }
        if Instant::now() > deadline {
            return Err(format!("waited for {what}"));
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// The real `fux attach`, on a PTY of its own, and a fux-vt screen of what it
/// shows.
pub struct Terminal {
    /// `None` once the terminal is closed.
    master: Option<std::os::fd::OwnedFd>,
    pub child: Child,
    pub screen: fux_vt::Parser,
    pub output: Vec<u8>,
}

impl Terminal {
    pub fn attach(
        server: &Server,
        rows: u16,
        cols: u16,
        args: &[&str],
    ) -> Result<Terminal, String> {
        let argv: Vec<&str> = [FUX, "attach"]
            .into_iter()
            .chain(args.iter().copied())
            .collect();
        Terminal::start(rows, cols, &argv, |command| {
            command.env("FUX_SOCKET", &server.socket);
        })
    }

    /// `argv` on a PTY of its own, leading a new session with the PTY as its
    /// controlling terminal, as a terminal emulator starts its shell; `setup`
    /// adds to its environment.
    pub fn start(
        rows: u16,
        cols: u16,
        argv: &[&str],
        setup: impl FnOnce(&mut Command),
    ) -> Result<Terminal, String> {
        let (master, slave) = fux::process::open_pty(rows, cols)?;
        let child = {
            let _guard = SPAWN.lock().map_err(e)?;
            fux::process::launch(Path::new(FUX), argv, &slave, |command| {
                command.env("TERM", "xterm-256color").env_remove("FUX_PANE");
                setup(command);
            })
            .map_err(e)?
        };
        drop(slave);
        Ok(Terminal {
            master: Some(master),
            child,
            screen: fux_vt::Parser::new(rows, cols, 0).map_err(e)?,
            output: Vec::new(),
        })
    }

    /// Closes the terminal, as closing its window does: the kernel hangs up
    /// the session it controls.
    pub fn hang_up(&mut self) {
        self.master = None;
    }

    pub fn pump(&mut self) {
        let Some(master) = &self.master else {
            return;
        };
        let mut buffer = vec![0u8; 64 * 1024];
        while let Ok(n) = fuxix::io::read(master, buffer.as_mut_slice()) {
            if n == 0 {
                break;
            }
            let bytes = buffer.get(..n).unwrap_or_default();
            self.output.extend_from_slice(bytes);
            let _ = self.screen.process(bytes);
        }
    }

    pub fn text(&self) -> String {
        let screen = self.screen.screen();
        let (rows, cols) = screen.size();
        let window = screen.window(0, rows, cols);
        (0..rows)
            .map(|y| {
                window
                    .row(y)
                    .map(|r| {
                        let mut line = String::new();
                        for cell in r.cells {
                            if !cell.is_wide_continuation() {
                                line.push_str(if cell.has_contents() {
                                    cell.contents()
                                } else {
                                    " "
                                });
                            }
                        }
                        line.trim_end().to_owned()
                    })
                    .unwrap_or_default()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    pub fn wait_for(&mut self, needle: &str) -> Outcome {
        let deadline = after(PATIENCE);
        loop {
            self.pump();
            if self.text().contains(needle)
                || String::from_utf8_lossy(&self.output).contains(needle)
            {
                return Ok(());
            }
            if Instant::now() > deadline {
                return Err(format!(
                    "waited for {needle:?}; the terminal shows:\n{}",
                    self.text()
                ));
            }
            std::thread::sleep(Duration::from_millis(15));
        }
    }

    pub fn type_bytes(&mut self, bytes: &[u8]) -> Outcome {
        let master = self.master.as_ref().ok_or("the terminal is closed")?;
        let mut rest = bytes;
        while !rest.is_empty() {
            match fuxix::io::write(master, rest) {
                Ok(n) => rest = rest.get(n..).unwrap_or_default(),
                Err(fuxix::Errno::AGAIN | fuxix::Errno::INTR) => {
                    std::thread::sleep(Duration::from_millis(1))
                }
                Err(error) => return Err(e(error)),
            }
        }
        Ok(())
    }

    pub fn resize(&mut self, rows: u16, cols: u16) -> Outcome {
        let master = self.master.as_ref().ok_or("the terminal is closed")?;
        fux::process::resize(master, rows, cols);
        self.screen = fux_vt::Parser::new(rows, cols, 0).map_err(e)?;
        // The kernel signals the foreground group of the PTY: the client.
        Ok(())
    }

    /// Waits for the client to exit; its status.
    pub fn wait_exit(&mut self) -> Result<std::process::ExitStatus, String> {
        let deadline = after(PATIENCE);
        loop {
            self.pump();
            if let Some(status) = self.child.try_wait().map_err(e)? {
                self.pump();
                return Ok(status);
            }
            if Instant::now() > deadline {
                return Err(format!(
                    "the client did not exit; it shows:\n{}",
                    self.text()
                ));
            }
            std::thread::sleep(Duration::from_millis(15));
        }
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
