//! A real server, real `fux` CLI calls, and scripted attach clients that
//! speak the protocol and keep a fux-vt screen of what they were painted.
#![allow(
    dead_code,
    reason = "each test binary uses a different part of this module"
)]
use fux::act::Act;
use fux::client::Keyboard;
use fux::keymap::Keymap;
use fux::protocol::{
    Attach, AttachedFrame, Decoder, Frame, Greeting, Hello, PROTOCOL, Role, ToAttached,
};
use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

pub type Outcome = Result<(), String>;

/// A size a frame carries: never zero.
pub fn sized(rows: u16, cols: u16) -> Result<fux_vt::Size, String> {
    fux_vt::Size::new(rows, cols).map_err(|_| "a size of zero".to_owned())
}

pub const FUX: &str = env!("CARGO_BIN_EXE_fux");

/// The fux binary the shared server runs: `FUX_BIN` if set, to measure
/// another build with the same tests (the bench harness does), else this
/// build's.
pub fn fux() -> &'static str {
    static BIN: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    BIN.get_or_init(|| std::env::var("FUX_BIN").unwrap_or_else(|_| FUX.to_owned()))
}
/// How long a test waits for something to show.
pub const PATIENCE: Duration = Duration::from_secs(10);

static COUNT: AtomicUsize = AtomicUsize::new(0);
/// Servers are started one at a time, so no fork in one test can inherit
/// another test's descriptors.
static SPAWN: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Holds the lock every server start takes (`SPAWN`), for a test that
/// starts one of its own, until the guard is dropped.
pub fn spawning() -> Result<std::sync::MutexGuard<'static, ()>, String> {
    SPAWN.lock().map_err(e)
}

/// When a wait that starts now gives up. A time past what an `Instant`
/// holds gives up at once.
pub fn after(wait: Duration) -> Instant {
    let now = Instant::now();
    now.checked_add(wait).unwrap_or(now)
}

pub fn e(error: impl std::fmt::Display) -> String {
    error.to_string()
}

/// A terminal of `rows` by `cols`, without history.
fn terminal(rows: u16, cols: u16) -> Result<fux_vt::Parser, String> {
    fux_vt::Parser::new(fux_vt::Size::new(rows, cols).map_err(e)?, 0).map_err(e)
}

pub fn pty_size(rows: u16, cols: u16) -> Result<fuxix::terminal::Size, String> {
    fuxix::terminal::Size::new(rows, cols).ok_or_else(|| format!("a {rows}x{cols} terminal"))
}

pub struct Server {
    pub dir: PathBuf,
    pub socket: PathBuf,
    child: Option<Child>,
    /// Its clients' keys: the defaults, and what the config binds.
    pub keymap: Keymap,
}

pub struct Output {
    pub status: i32,
    pub stdout: String,
    pub stderr: String,
}

fn output(out: std::process::Output) -> Result<Output, String> {
    Ok(Output {
        status: out.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    })
}

impl Server {
    /// A server whose shell is `sh` with a plain prompt, and `config`, a
    /// fuxth config its clients run: what it `set`s, set as it starts.
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
        let base = fuxix::file::scratch().map_err(e)?;
        let dir = base.join(format!(
            "fux-t{}-{}",
            std::process::id(),
            COUNT.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).map_err(e)?;
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).map_err(e)?;
        let config_dir = dir.join("config").join("fux");
        std::fs::create_dir_all(&config_dir).map_err(e)?;
        let config_path = config_dir.join("config.fux");
        std::fs::write(&config_path, format!("{config}\n")).map_err(e)?;
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
                        .arg(fux());
                    sh
                }
                None => Command::new(fux()),
            };
            let child = command
                .arg(fux::SERVE)
                .arg(&socket)
                .env("PS1", "$ ")
                .env("SHELL", "/bin/sh")
                .env("ENV", "/dev/null")
                .env("HOME", &dir)
                .env_remove("FUX_PANE")
                .env_remove("FUX_SERVER")
                // A stale one: its panes are given the server's own.
                .env("FUX", "/nonexistent/fux")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(log)
                .spawn()
                .map_err(e)?;
            // Closed here once the server has its copy.
            let held = leaked.as_ref().map(std::os::fd::AsRawFd::as_raw_fd);
            (child, held)
        };
        let mut server = Server {
            dir,
            socket,
            child: Some(child),
            keymap: Keymap::empty(),
        };
        let deadline = after(PATIENCE);
        while UnixStream::connect(&server.socket).is_err() {
            if Instant::now() > deadline {
                return Err(format!("the server did not start: {}", server.log()));
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        // The config, as a client runs it: its settings to the server, its
        // bindings to the keymap the scripted clients type through.
        let socket = fux::socket::Choice::Path(server.socket.clone());
        let mut to = fux::script::Server::new(socket);
        server.keymap = fux::script::run_file(&config_path, Some(&mut to)).map_err(e)?;
        // A server starts with nothing in it; the tests' first shell, %1 in
        // workspace main, is 80 by 24, in the server's home.
        let home = server.dir.to_string_lossy().into_owned();
        let first = [
            "opts", "str", "main", "name", "80", "24", "size", "str", &home, "cwd",
        ];
        server.ok(&[&first[..], &["new-workspace"]].concat())?;
        Ok((server, held))
    }

    /// Its socket, as fux takes it.
    pub fn socket_path(&self) -> Result<fux::socket::SocketPath, String> {
        fux::socket::socket_path(&fux::socket::Choice::Path(self.socket.clone())).map_err(e)
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
        let mut command = self.command(fux());
        command.args(args);
        for (k, v) in env {
            command.env(k, v);
        }
        output(command.output().map_err(e)?)
    }

    /// `program`, to run against this server.
    fn command(&self, program: impl AsRef<std::ffi::OsStr>) -> Command {
        let mut command = Command::new(program);
        command
            .env("FUX_SOCKET", &self.socket)
            .env("XDG_CONFIG_HOME", self.dir.join("config"))
            .env_remove("FUX_PANE")
            .env_remove("FUX_SERVER");
        command
    }

    /// Runs `program` with `args`, `stdin` piped to it, as a shell runs
    /// it: a script's `#!` line finds this build's fux first on the PATH.
    pub fn piped(&self, program: &Path, args: &[&str], stdin: &str) -> Result<Output, String> {
        let bin = Path::new(fux()).parent().ok_or("fux is in no directory")?;
        let path = std::env::var_os("PATH").unwrap_or_default();
        let path = std::iter::once(bin.to_owned()).chain(std::env::split_paths(&path));
        let mut command = self.command(program);
        (command.args(args))
            .env("PATH", std::env::join_paths(path).map_err(e)?)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        // A fork in another test may hold a script just written open for
        // writing a moment longer (ETXTBSY).
        let mut child = command.spawn();
        for _ in 0..50 {
            match &child {
                Err(busy) if busy.kind() == std::io::ErrorKind::ExecutableFileBusy => {
                    std::thread::sleep(Duration::from_millis(20));
                    child = command.spawn();
                }
                _ => break,
            }
        }
        let mut child = child.map_err(e)?;
        (child.stdin.take().ok_or("no stdin")?)
            .write_all(stdin.as_bytes())
            .map_err(e)?;
        output(child.wait_with_output().map_err(e)?)
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

    /// The version of `pane` (`PANE:RESPAWNS.SCREEN.BELLS.DONE.ENDED:RUN`).
    pub fn version(&self, pane: &str) -> Result<String, String> {
        Ok(self
            .ok(&["str", pane, "pane", "str", "version", "get"])?
            .trim()
            .to_owned())
    }

    /// Waits, as `docs/agents.md`'s recipes do, from version `since` until
    /// `done` holds of `pane`'s whole capture; its capture then. Fails if
    /// its program ends first, or after `PATIENCE`.
    pub fn until(
        &self,
        pane: &str,
        since: &str,
        done: impl Fn(&str) -> bool,
    ) -> Result<String, String> {
        let deadline = after(PATIENCE);
        let mut since = since.to_owned();
        loop {
            let capture = self.ok(&["str", pane, "pane", "all-history", "capture"])?;
            if done(&capture) {
                return Ok(capture);
            }
            if ended(&since) {
                return Err(format!("{pane} ended first, at {since}: {capture}"));
            }
            since = self.changed(pane, &since, deadline)?;
        }
    }

    /// Waits from `since` until `pane`'s version differs, by `deadline`: the
    /// new version. Unchanged in one wait (124), it waits again.
    pub fn changed(&self, pane: &str, since: &str, deadline: Instant) -> Result<String, String> {
        loop {
            let left = deadline.saturating_duration_since(Instant::now()).as_secs();
            if left == 0 {
                return Err(format!("{pane} unchanged at {since}"));
            }
            let left = left.to_string();
            let waited = ["str", since, "changed", &left, "timeout", "wait"];
            let out = self.fux(&waited)?;
            match out.status {
                0 => {
                    let mut words = out.stdout.split_whitespace();
                    return Ok(words.next().unwrap_or_default().to_owned());
                }
                124 => continue,
                _ => return Err(format!("{pane}: {}", out.stderr)),
            }
        }
    }

    /// Waits, from now, until `pane`'s capture holds `text`; its capture.
    pub fn shows(&self, pane: &str, text: &str) -> Result<String, String> {
        let since = self.version(pane)?;
        self.until(pane, &since, |capture| capture.contains(text))
    }

    /// Waits until `pane`'s program has ended, as its version says, a
    /// minute at most (a test's program may take a while on a slow
    /// machine): its `exited` then (`3`, or `null` if a signal killed it).
    pub fn exits(&self, pane: &str) -> Result<String, String> {
        let deadline = after(Duration::from_secs(60));
        let mut since = self.version(pane)?;
        while !ended(&since) {
            since = self.changed(pane, &since, deadline)?;
        }
        Ok(self
            .ok(&["str", pane, "pane", "str", "exited", "get"])?
            .trim()
            .to_owned())
    }

    pub fn attach(&self, rows: u16, cols: u16) -> Result<Client, String> {
        Client::connect(self, (rows, cols), None, false)
    }

    /// A client attached to `workspace`.
    pub fn attach_to(&self, rows: u16, cols: u16, workspace: &str) -> Result<Client, String> {
        Client::connect(self, (rows, cols), Some(workspace), false)
    }

    /// A client that only watches (`opts watch attach`).
    pub fn watch(&self, rows: u16, cols: u16) -> Result<Client, String> {
        Client::connect(self, (rows, cols), None, true)
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
    #[expect(
        clippy::print_stderr,
        reason = "the harness captures eprintln! alone, showing it for a failing test"
    )]
    fn drop(&mut self) {
        // Test output is shown only for a failing test, so the server's log
        // is always printed; it explains a failure the assertion cannot.
        eprintln!(
            "--- server log ---\n{}--- end of server log ---",
            self.log()
        );
        if let Some(mut child) = self.child.take() {
            let _ = Command::new(fux())
                .args(["force-kill-server"])
                .env("FUX_SOCKET", &self.socket)
                .env_remove("FUX_PANE")
                .env_remove("FUX_SERVER")
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

/// A scripted attach client: its keys typed through fux's own keyboard
/// (`fux::client::Keyboard`, as the real client's are), what it sends, and a
/// terminal of what it was sent.
pub struct Client {
    stream: UnixStream,
    decoder: Decoder,
    socket: fux::socket::SocketPath,
    keymap: Keymap,
    read_only: bool,
    /// Whether the server's Hello has come.
    greeted: bool,
    /// Its keys, once the server has said its id.
    keyboard: Option<Keyboard>,
    /// Bytes typed before then, typed once it has.
    typed: Vec<u8>,
    pub id: Option<String>,
    pub terminal: fux_vt::Parser,
    pub rows: u16,
    pub cols: u16,
    pub exit: Option<String>,
    /// Every byte of every paint, for tests that look at the sequences.
    pub painted: Vec<u8>,
}

impl Client {
    /// A client attached to `workspace`, or the first; with `read_only`,
    /// one that only watches.
    pub fn connect(
        server: &Server,
        (rows, cols): (u16, u16),
        workspace: Option<&str>,
        read_only: bool,
    ) -> Result<Client, String> {
        let mut stream = UnixStream::connect(&server.socket).map_err(e)?;
        let hello = Hello {
            protocol: PROTOCOL,
            version: "test".into(),
            role: Role::Attach,
        };
        stream.write_all(&hello.encode().map_err(e)?).map_err(e)?;
        let attach = Attach {
            size: sized(rows, cols)?,
            workspace: workspace.map(|name| fux::command::WsRef::Name(name.to_owned())),
            read_only,
            env: Vec::new(),
        };
        stream.write_all(&attach.encode().map_err(e)?).map_err(e)?;
        stream.set_nonblocking(true).map_err(e)?;
        Ok(Client {
            stream,
            decoder: Decoder::default(),
            socket: server.socket_path()?,
            keymap: server.keymap.clone(),
            read_only,
            greeted: false,
            keyboard: None,
            typed: Vec::new(),
            id: None,
            terminal: terminal(rows, cols)?,
            rows,
            cols,
            exit: None,
            painted: Vec::new(),
        })
    }

    /// Bytes typed at its terminal: through its keys, as the real client
    /// takes them.
    pub fn send(&mut self, bytes: &[u8]) -> Outcome {
        if self.keyboard.is_none() {
            self.typed.extend_from_slice(bytes);
            return self.pump();
        }
        let acts = match &mut self.keyboard {
            Some(keyboard) => keyboard.bytes(bytes, Instant::now()),
            None => Vec::new(),
        };
        self.acts(acts)?;
        // Done with them before the next keys, at a person's pace: the
        // programs they ran, and the keys held for them.
        eventually("the keys taken", || {
            self.pump()?;
            Ok(self.keyboard.as_ref().is_none_or(Keyboard::idle))
        })
    }

    /// Types text, as keys.
    pub fn keys(&mut self, text: &str) -> Outcome {
        self.send(text.as_bytes())
    }

    /// An action, sent as it is, as a client that made it of its keys.
    pub fn act(&mut self, act: Act) -> Outcome {
        self.frame(&AttachedFrame::Act(Box::new(act)))
    }

    fn acts(&mut self, acts: Vec<Act>) -> Outcome {
        for act in acts {
            self.act(act)?;
        }
        Ok(())
    }

    pub fn resize(&mut self, rows: u16, cols: u16) -> Outcome {
        self.rows = rows;
        self.cols = cols;
        self.terminal = terminal(rows, cols)?;
        self.frame(&AttachedFrame::Resize(sized(rows, cols)?))
    }

    pub fn frame<'a>(&mut self, frame: &impl Frame<'a>) -> Outcome {
        self.write(&frame.encode().map_err(e)?)
    }

    /// Sends any bytes, as a client that does not follow the protocol
    /// might.
    pub fn write(&mut self, bytes: &[u8]) -> Outcome {
        self.stream.set_nonblocking(false).map_err(e)?;
        let result = self.stream.write_all(bytes).map_err(e);
        self.stream.set_nonblocking(true).map_err(e)?;
        result
    }

    pub fn detach(&mut self) -> Outcome {
        self.frame(&AttachedFrame::Detach)
    }

    /// Reads whatever the server has sent, and does what its keys owe: the
    /// programs they ran told of, a lone Escape taken once its wait passes.
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
        let mut acts = Vec::new();
        // The server's Hello, or why there is none, then what it sends an
        // attached client.
        while !self.greeted {
            match self.decoder.frame::<Greeting>().map_err(e)? {
                Some(Greeting::Hello(_)) => self.greeted = true,
                Some(Greeting::Exit(reason)) => self.exit = Some(reason.to_owned()),
                Some(Greeting::Done(_)) | None => break,
            }
        }
        while self.greeted
            && let Some(frame) = self.decoder.frame().map_err(e)?
        {
            match frame {
                ToAttached::Paint(bytes) => {
                    self.painted.extend_from_slice(&bytes);
                    self.terminal.process(&bytes).map_err(e)?;
                }
                ToAttached::Exit(reason) => self.exit = Some(reason.to_owned()),
                ToAttached::Welcome(me) if self.keyboard.is_none() => {
                    let keymap = self.keymap.clone();
                    self.keyboard =
                        Some(Keyboard::new(&self.socket, me, keymap, self.read_only).map_err(e)?);
                    self.id = Some(me.to_string());
                }
                frame @ (ToAttached::Welcome(..)
                | ToAttached::Ended(_)
                | ToAttached::Run(_)
                | ToAttached::Keys(_)) => {
                    if let Some(keyboard) = &mut self.keyboard {
                        acts.extend(keyboard.frame(frame));
                    }
                }
            }
        }
        if let Some(keyboard) = &mut self.keyboard {
            acts.extend(keyboard.ran());
            if keyboard.deadline().is_some_and(|at| at <= Instant::now()) {
                acts.extend(keyboard.timeout());
            }
            if !self.typed.is_empty() {
                let typed = std::mem::take(&mut self.typed);
                acts.extend(keyboard.bytes(&typed, Instant::now()));
            }
        }
        self.acts(acts)
    }

    /// Waits until the server has said the client's id, so that its keys
    /// run.
    pub fn ready(&mut self) -> Outcome {
        eventually("the client's id", || {
            self.pump()?;
            Ok(self.keyboard.is_some())
        })
    }

    /// The screen's rows, as text.
    pub fn lines(&self) -> Vec<String> {
        let window = self.terminal.screen().window();
        (0..self.rows)
            .map(|y| {
                let mut line = String::new();
                if let Some(row) = window.row(y) {
                    for cell in row.cells() {
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

/// A process's resident memory, in MiB, from `ps`.
pub fn resident_mib(pid: u32) -> Result<f64, String> {
    let out = Command::new("ps")
        .args(["-o", "rss=", "-p", &pid.to_string()])
        .output()
        .map_err(e)?;
    let kib: f64 = String::from_utf8_lossy(&out.stdout)
        .trim()
        .parse()
        .map_err(|error| format!("ps rss: {error}"))?;
    Ok(kib / 1024.0)
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
            Ok(self.ok(&["str", pane, "pane", "capture"])?.contains('$'))
        })?;
        self.ok(&[
            "str",
            pane,
            "pane",
            "str",
            line,
            "text",
            "str",
            "Enter",
            "key",
            "send-keys",
        ])?;
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

/// The real `fux` attached (no words, or `opts … attach`), on a PTY of
/// its own, and a fux-vt screen of what it shows.
pub struct Terminal {
    /// `None` once the terminal is closed.
    master: Option<fuxix::pty::Master>,
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
        let argv: Vec<&str> = [fux()].into_iter().chain(args.iter().copied()).collect();
        Terminal::start(rows, cols, &argv, |command| {
            command
                .env("FUX_SOCKET", &server.socket)
                .env("XDG_CONFIG_HOME", server.dir.join("config"))
                .env_remove("FUX_PANE")
                .env_remove("FUX_SERVER");
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
        let (master, slave) = fuxix::pty::open(pty_size(rows, cols)?).map_err(e)?;
        let (child, _) = {
            let _guard = SPAWN.lock().map_err(e)?;
            slave
                .spawn(argv, |command| {
                    command
                        .env("TERM", "xterm-256color")
                        .env_remove("FUX_PANE")
                        .env_remove("FUX_SERVER");
                    setup(command);
                })
                .map_err(e)?
        };
        Ok(Terminal {
            master: Some(master),
            child,
            screen: terminal(rows, cols)?,
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
        let window = self.screen.screen().window();
        (0..window.rows())
            .map(|y| {
                window
                    .row(y)
                    .map(|r| {
                        let mut line = String::new();
                        for cell in r.cells() {
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
        fuxix::terminal::set_window_size(master, pty_size(rows, cols)?, None).map_err(e)?;
        self.screen = terminal(rows, cols)?;
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
        // The terminal closed first: on macOS a session leader's exit
        // waits for its terminal's output to be read (xnu's `proc_exit`
        // calls `ttywait`), and nothing reads it here.
        self.master = None;
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// The pane client c1 has in focus, as `fux ls` says.
pub fn focused(server: &Server) -> Result<String, String> {
    let ls = server.ok(&["ls"])?;
    let client = ls
        .lines()
        .find(|l| l.starts_with("client c1"))
        .ok_or("no client")?;
    Ok(client
        .split_whitespace()
        .last()
        .unwrap_or_default()
        .to_owned())
}

/// What a client was painted, as text.
pub fn painted(client: &Client) -> String {
    String::from_utf8_lossy(&client.painted).into_owned()
}

/// Waits until `what` was painted `count` times.
pub fn wait_painted(client: &mut Client, what: &str, count: usize) -> Outcome {
    eventually(&format!("{count} of {what:?} painted"), || {
        client.pump()?;
        Ok(painted(client).matches(what).count() >= count)
    })
}

/// The `n`th of a version's counts, from 0 (`RESPAWNS`), as it is written.
pub fn count(version: &str, n: usize) -> Option<&str> {
    version.split(':').nth(1)?.split('.').nth(n)
}

/// Whether `version` says its program ended (`ENDED`, its last count, 1).
pub fn ended(version: &str) -> bool {
    count(version, 4) == Some("1")
}
