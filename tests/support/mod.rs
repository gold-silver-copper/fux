//! A real server, real `fux` CLI calls, and scripted attach clients that
//! speak the protocol and keep a fux-vt screen of what they were painted.
#![allow(dead_code)]
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
        let child = {
            let _guard = SPAWN.lock().map_err(e)?;
            Command::new(FUX)
                .arg("server")
                .arg("--socket")
                .arg(&socket)
                .arg("--config")
                .arg(&config_path)
                .env("PS1", "$ ")
                .env("ENV", "/dev/null")
                .env("HOME", &dir)
                .env_remove("FUX_PANE")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(log)
                .spawn()
                .map_err(e)?
        };
        let server = Server {
            dir,
            socket,
            child: Some(child),
        };
        let deadline = Instant::now() + PATIENCE;
        while UnixStream::connect(&server.socket).is_err() {
            if Instant::now() > deadline {
                return Err(format!("the server did not start: {}", server.log()));
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        Ok(server)
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
        let deadline = Instant::now() + PATIENCE;
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
            let deadline = Instant::now() + Duration::from_secs(3);
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
        let mut buffer = [0u8; 65536];
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
                _ => {}
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
        let deadline = Instant::now() + PATIENCE;
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
        let deadline = Instant::now() + PATIENCE;
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

/// Waits until `test` holds, polling.
pub fn eventually(what: &str, mut test: impl FnMut() -> Result<bool, String>) -> Outcome {
    let deadline = Instant::now() + PATIENCE;
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
    pub master: std::os::fd::OwnedFd,
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
        let (master, slave) = fux::process::open_pty(rows, cols)?;
        let stdio = |fd: &std::os::fd::OwnedFd| fd.try_clone().map(Stdio::from).map_err(e);
        let child = {
            let _guard = SPAWN.lock().map_err(e)?;
            let mut command = Command::new(FUX);
            command
                .arg("attach")
                .args(args)
                .env("FUX_SOCKET", &server.socket)
                .env("TERM", "xterm-256color")
                .env_remove("FUX_PANE")
                .stdin(stdio(&slave)?)
                .stdout(stdio(&slave)?)
                .stderr(stdio(&slave)?);
            // SAFETY: setsid and TIOCSCTTY are async-signal-safe system calls.
            unsafe {
                use std::os::unix::process::CommandExt;
                command.pre_exec(|| {
                    rustix::process::setsid().map_err(std::io::Error::from)?;
                    rustix::process::ioctl_tiocsctty(rustix::stdio::stdin())
                        .map_err(std::io::Error::from)?;
                    Ok(())
                });
            }
            command.spawn().map_err(e)?
        };
        drop(slave);
        Ok(Terminal {
            master,
            child,
            screen: fux_vt::Parser::new(rows, cols, 0).map_err(e)?,
            output: Vec::new(),
        })
    }

    pub fn pump(&mut self) {
        let mut buffer = [0u8; 65536];
        while let Ok(n) = rustix::io::read(&self.master, &mut buffer) {
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
        let deadline = Instant::now() + PATIENCE;
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
        let mut rest = bytes;
        while !rest.is_empty() {
            match rustix::io::write(&self.master, rest) {
                Ok(n) => rest = rest.get(n..).unwrap_or_default(),
                Err(rustix::io::Errno::AGAIN | rustix::io::Errno::INTR) => {
                    std::thread::sleep(Duration::from_millis(1))
                }
                Err(error) => return Err(e(error)),
            }
        }
        Ok(())
    }

    pub fn resize(&mut self, rows: u16, cols: u16) -> Outcome {
        fux::process::resize(&self.master, rows, cols);
        self.screen = fux_vt::Parser::new(rows, cols, 0).map_err(e)?;
        // The kernel signals the foreground group of the PTY: the client.
        Ok(())
    }

    /// Waits for the client to exit; its status.
    pub fn wait_exit(&mut self) -> Result<std::process::ExitStatus, String> {
        let deadline = Instant::now() + PATIENCE;
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
