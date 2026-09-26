//! One walk's world: a fresh server in a private directory, real clients on
//! PTYs of their own, and the script panes act through. Only what it started
//! is ever signalled, every wait is bounded, and cleanup runs however the
//! walk ends.
use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::os::fd::OwnedFd;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// How long one CLI call may take.
const CALL: Duration = Duration::from_secs(10);

pub fn after(wait: Duration) -> Instant {
    let now = Instant::now();
    now.checked_add(wait).unwrap_or(now)
}

/// The script a pane runs to act on its own, and to say it is done with a
/// marker assembled when it runs, so the typed command never shows it.
const ACT: &str = r#"#!/bin/sh
# act.sh ACT [ARGS] SERIAL
case "$1" in
  alternate) printf '\033[?1049h'; echo alternate-screen; sleep 0.2; printf '\033[?1049l' ;;
  mouse) printf '\033[?1000h\033[?1006h' ;;
  bracketed) printf '\033[?2004h' ;;
  appcursor) printf '\033[?1h\033=' ;;
  stty) stty rows "$2" cols "$3"; shift 2 ;;
  burst) i=0; while [ "$i" -lt "$2" ]; do echo "burst line $i of $2"; i=$((i+1)); done; shift ;;
  deaf) sleep "$2"; shift ;;
esac
shift
echo "act-done-$(( $1 + 1000 ))"
"#;

/// A real `fux attach` on a PTY, and a terminal of what it shows.
pub struct Client {
    pub id: String,
    master: Option<OwnedFd>,
    pub child: Child,
    pub screen: fux_vt::Parser,
    pub rows: u16,
    pub cols: u16,
}

impl Client {
    /// Reads what the client wrote to its terminal.
    pub fn pump(&mut self) {
        let Some(master) = &self.master else {
            return;
        };
        let mut buffer = vec![0u8; 64 * 1024];
        while let Ok(n) = fuxix::io::read(master, buffer.as_mut_slice()) {
            if n == 0 {
                break;
            }
            let _ = self.screen.process(buffer.get(..n).unwrap_or_default());
        }
    }

    /// Types bytes, waiting while the terminal's input is full.
    pub fn type_bytes(&mut self, bytes: &[u8]) -> Result<(), String> {
        let master = self.master.as_ref().ok_or("the terminal is closed")?;
        let mut rest = bytes;
        let deadline = after(CALL);
        while !rest.is_empty() {
            match fuxix::io::write(master, rest) {
                Ok(n) => rest = rest.get(n..).unwrap_or_default(),
                Err(fuxix::Errno::AGAIN | fuxix::Errno::INTR) => {
                    if Instant::now() > deadline {
                        return Err("the client's terminal stayed full".into());
                    }
                    std::thread::sleep(Duration::from_millis(1));
                }
                Err(error) => return Err(error.to_string()),
            }
        }
        Ok(())
    }

    /// Resizes the terminal; the kernel signals the client.
    pub fn resize(&mut self, rows: u16, cols: u16) -> Result<(), String> {
        let master = self.master.as_ref().ok_or("the terminal is closed")?;
        fuxix::terminal::set_window_size(master, rows, cols).map_err(|e| e.to_string())?;
        // The same size signals nothing, and nothing is repainted: the
        // screen stays as it is.
        if (rows, cols) == (self.rows, self.cols) {
            return Ok(());
        }
        self.screen = fux_vt::Parser::new(rows, cols, 0).map_err(|e| e.to_string())?;
        self.rows = rows;
        self.cols = cols;
        Ok(())
    }

    /// Closes the terminal, as closing its window does.
    pub fn hang_up(&mut self) {
        self.master = None;
        if let Some(pid) = fuxix::process::Pid::of(&self.child) {
            let _ = fuxix::process::kill(pid, fuxix::process::Signal::Hup);
        }
    }

    /// The screen's rows, as `capture-client` prints them.
    pub fn lines(&self) -> Vec<String> {
        let screen = self.screen.screen();
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

    /// Whether it has exited.
    pub fn gone(&mut self) -> bool {
        self.child.try_wait().ok().flatten().is_some()
    }
}

/// The outcome of a CLI call.
pub struct Output {
    pub status: i32,
    pub stdout: String,
    pub stderr: String,
}

pub struct Fixture {
    pub dir: PathBuf,
    pub socket: PathBuf,
    pub config: PathBuf,
    pub fux: PathBuf,
    server: Option<Child>,
    pub clients: BTreeMap<String, Client>,
    serial: u32,
}

impl Fixture {
    /// A fresh server, in a fresh 0700 directory, with one client.
    pub fn start(fux: &Path) -> Result<Fixture, String> {
        // Under /tmp, not $TMPDIR: a socket path is at most about 100
        // bytes, and macOS's $TMPDIR alone is half of that.
        static COUNT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let dir = Path::new("/tmp")
            .canonicalize()
            .map_err(|e| e.to_string())?
            .join(format!(
                "fux-walk-{}-{}",
                std::process::id(),
                COUNT.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
            ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))
            .map_err(|e| e.to_string())?;
        std::fs::write(dir.join("act.sh"), ACT).map_err(|e| e.to_string())?;
        let config = dir.join("fux.conf");
        std::fs::write(&config, CONFIG_VALID).map_err(|e| e.to_string())?;
        let socket = dir.join("s").join("fux.sock");
        let log = std::fs::File::create(dir.join("server.log")).map_err(|e| e.to_string())?;
        let mut server = Command::new(fux);
        server
            .arg("server")
            .arg("--socket")
            .arg(&socket)
            .arg("--config")
            .arg(&config)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(log);
        environment(&mut server, &dir);
        let server = server.spawn().map_err(|e| e.to_string())?;
        let mut fixture = Fixture {
            dir,
            socket,
            config,
            fux: fux.to_owned(),
            server: Some(server),
            clients: BTreeMap::new(),
            serial: 0,
        };
        let deadline = after(CALL);
        while UnixStream::connect(&fixture.socket).is_err() {
            if Instant::now() > deadline {
                return Err(format!("the server did not start: {}", fixture.log()));
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        fixture.attach(24, 80)?;
        Ok(fixture)
    }

    pub fn log(&self) -> String {
        std::fs::read_to_string(self.dir.join("server.log")).unwrap_or_default()
    }

    /// Whether the server still runs.
    pub fn server_alive(&mut self) -> bool {
        self.server
            .as_mut()
            .is_some_and(|s| s.try_wait().ok().flatten().is_none())
    }

    /// `fux ARGS…` against this server, within `CALL`.
    pub fn fux(&self, args: &[&str]) -> Result<Output, String> {
        self.fux_within(args, CALL)
    }

    /// `fux ARGS…` against this server, within `limit`.
    pub fn fux_within(&self, args: &[&str], limit: Duration) -> Result<Output, String> {
        let mut command = Command::new(&self.fux);
        command
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        environment(&mut command, &self.dir);
        command.env("FUX_SOCKET", &self.socket);
        let mut child = command.spawn().map_err(|e| e.to_string())?;
        let (mut out, mut err) = (child.stdout.take(), child.stderr.take());
        // Read on threads: a large answer would otherwise fill the pipe
        // before the wait sees the exit.
        let reader = std::thread::spawn(move || {
            let mut text = String::new();
            if let Some(out) = out.as_mut() {
                let _ = out.read_to_string(&mut text);
            }
            text
        });
        let error_reader = std::thread::spawn(move || {
            let mut text = String::new();
            if let Some(err) = err.as_mut() {
                let _ = err.read_to_string(&mut text);
            }
            text
        });
        let deadline = after(limit);
        let status = loop {
            if let Some(status) = child.try_wait().map_err(|e| e.to_string())? {
                break status;
            }
            if Instant::now() > deadline {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!(
                    "`fux {}` took longer than {limit:?}",
                    args.join(" ")
                ));
            }
            std::thread::sleep(Duration::from_millis(2));
        };
        Ok(Output {
            status: status.code().unwrap_or(-1),
            stdout: reader.join().unwrap_or_default(),
            stderr: error_reader.join().unwrap_or_default(),
        })
    }

    /// The client ids `ls` lists.
    pub fn listed_clients(&self) -> Result<Vec<String>, String> {
        Ok(self
            .fux(&["ls"])?
            .stdout
            .lines()
            .filter_map(|l| l.strip_prefix("client "))
            .filter_map(|l| l.split_whitespace().next())
            .map(str::to_owned)
            .collect())
    }

    /// A new real client of `rows` by `cols`: its id.
    pub fn attach(&mut self, rows: u16, cols: u16) -> Result<String, String> {
        let before = self.listed_clients()?;
        let (master, slave) = fux::process::open_pty(rows, cols)?;
        let argv = [self.fux.as_os_str(), std::ffi::OsStr::new("attach")];
        let socket = self.socket.clone();
        let dir = self.dir.clone();
        let child = fux::process::launch(&self.fux, &argv, &slave, |command| {
            environment(command, &dir);
            command.env("FUX_SOCKET", &socket);
        })
        .map_err(|e| e.to_string())?;
        drop(slave);
        let deadline = after(CALL);
        let id = loop {
            if let Some(id) = self
                .listed_clients()?
                .into_iter()
                .find(|c| !before.contains(c) && !self.clients.contains_key(c))
            {
                break id;
            }
            if Instant::now() > deadline {
                return Err("a new client never attached".into());
            }
            std::thread::sleep(Duration::from_millis(5));
        };
        self.clients.insert(
            id.clone(),
            Client {
                id: id.clone(),
                master: Some(master),
                child,
                screen: fux_vt::Parser::new(rows.max(1), cols.max(1), 0)
                    .map_err(|e| e.to_string())?,
                rows: rows.max(1),
                cols: cols.max(1),
            },
        );
        Ok(id)
    }

    /// A client that asks for 9000 by 9000, speaking the protocol, for as
    /// long as `ls` takes to show it: the size it got.
    pub fn clamped(&self) -> Result<(u16, u16), String> {
        use fux::protocol::{Frame, PROTOCOL, Role};
        let before = self.listed_clients()?;
        let mut stream = UnixStream::connect(&self.socket).map_err(|e| e.to_string())?;
        let hello = Frame::Hello {
            protocol: PROTOCOL,
            version: "walk".into(),
            role: Role::Attach,
        };
        stream
            .write_all(&hello.encode()?)
            .map_err(|e| e.to_string())?;
        let attach = Frame::Attach {
            rows: 9000,
            cols: 9000,
            workspace: None,
        };
        stream
            .write_all(&attach.encode()?)
            .map_err(|e| e.to_string())?;
        // Composing a 4096 by 4096 screen, some 16.7 million cells, takes
        // most of a second in a release build and seconds in a debug one.
        let patience = Duration::from_secs(60);
        let deadline = after(patience);
        let size = loop {
            let ls = self.fux_within(&["ls"], patience)?.stdout;
            if let Some(line) = ls.lines().find(|l| {
                l.strip_prefix("client ")
                    .and_then(|l| l.split_whitespace().next())
                    .is_some_and(|id| !before.iter().any(|b| b == id))
            }) {
                let size = line.split_whitespace().nth(2).unwrap_or_default();
                let (cols, rows) = size.split_once('x').unwrap_or_default();
                break (
                    rows.parse().map_err(|_| format!("a size in {line:?}"))?,
                    cols.parse().map_err(|_| format!("a size in {line:?}"))?,
                );
            }
            if Instant::now() > deadline {
                return Err("the 9000 by 9000 client never attached".into());
            }
            std::thread::sleep(Duration::from_millis(5));
        };
        let _ = stream.write_all(&Frame::Detach.encode()?);
        Ok(size)
    }

    /// A fresh serial for an act's marker.
    pub fn serial(&mut self) -> u32 {
        self.serial = self.serial.wrapping_add(1);
        self.serial
    }

    /// Ends everything this fixture started: its clients, then its server,
    /// asked first and killed if it will not go. The processes that remain
    /// are returned, as failures, never hidden.
    pub fn finish(&mut self, keep: bool) -> Vec<String> {
        let mut left = Vec::new();
        for client in self.clients.values_mut() {
            client.master = None;
            let _ = client.child.kill();
            let _ = client.child.wait();
        }
        let pids: Vec<i32> = crate::world::World::read(self)
            .map(|w| w.panes().map(|p| p.pid).collect())
            .unwrap_or_default();
        let _ = self.fux(&["kill-server"]);
        if let Some(mut server) = self.server.take() {
            let deadline = after(Duration::from_secs(5));
            while server.try_wait().ok().flatten().is_none() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(10));
            }
            if server.try_wait().ok().flatten().is_none() {
                left.push(format!("the server (pid {}) did not stop", server.id()));
                let _ = server.kill();
            }
            let _ = server.wait();
        }
        let deadline = after(Duration::from_secs(3));
        for pid in pids.into_iter().filter_map(fuxix::process::Pid::from_raw) {
            while crate::world::alive(pid) && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(10));
            }
            if crate::world::alive(pid) {
                left.push(format!("pane process {pid} outlived its server"));
            }
        }
        if !keep {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
        left
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        if self.server.is_some() {
            let _ = self.finish(true);
        }
    }
}

/// The configuration a walk starts from, and returns to.
pub const CONFIG_VALID: &str = "set shell /bin/sh\nset history-lines 500\n";

/// The environment of everything the walk starts: nothing inherited but
/// `PATH`, and `HOME` the fixture.
pub fn environment(command: &mut Command, dir: &Path) {
    command
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", dir)
        .env("PS1", "$ ")
        .env("ENV", "/dev/null")
        .env("SHELL", "/bin/sh")
        .env("TERM", "xterm-256color")
        .env("LANG", "C.UTF-8");
}
