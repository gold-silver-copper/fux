//! `feel`: what a person at the terminal feels, through real servers: fux,
//! tmux and zellij (if installed), each beside no multiplexer at all. Each
//! runs on a socket of its own, with a configuration of its own, and its
//! client on a PTY this process holds (`terminal`), as a terminal holds
//! it: the user's own servers are never touched. Wall time is what is
//! measured here; it is reported, not gated on.
//!
//! - **Latency:** a key typed at the client's PTY, through the server, to
//!   a pane program that answers it at once (`__echo`), and back in a paint
//!   at the client: from the write to the read that brings the answer's
//!   glyph. Keys are typed 17 to 25 ms apart, slower than fux paints
//!   (16 ms), as a person types, with no other output (idle) and while
//!   another pane beside it floods (`__flood`).
//! - **Throughput:** a pane program (`__serve`) writes a workload when
//!   asked, after a reset (RIS), then a marker no workload has: from the
//!   asking key to the read that brings the marker, when the client has
//!   the final screen (PR #78's end-to-end method). Every corpus recording
//!   and every synthetic workload, 16 MiB each. The bytes the client was
//!   sent, and the paints (fux begins each with `CSI ? 2026 h`; the
//!   others may not), give the bandwidth.
//! - **Footprint:** with the client idle at a shell, the CPU the server
//!   and client use in 10 s; the server's resident memory, then with 4
//!   more panes, then with 4 more whose 10,000 rows of history are full,
//!   then with 50 panes.
use crate::helpers;
use crate::terminal::Terminal;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Variables that would point a program at the user's own servers.
pub const FOREIGN: &[&str] = &[
    "FUX_SOCKET",
    "FUX_PANE",
    "TMUX",
    "TMUX_PANE",
    "ZELLIJ",
    "ZELLIJ_SESSION_NAME",
    "ZELLIJ_PANE_ID",
];

/// The pane's size, the corpus's.
const ROWS: u16 = 40;
const COLS: u16 = 120;
/// Rows of history a full pane has.
const HISTORY: u64 = 10_000;
/// How long a pane program may take to be ready, a workload to arrive.
const READY_WAIT: Duration = Duration::from_secs(30);
const LOAD_WAIT: Duration = Duration::from_secs(120);
const KEY_WAIT: Duration = Duration::from_secs(2);
/// A run of keys stops after this many go unanswered in a row.
const MISSES_IN_A_ROW: usize = 10;
/// Each synthetic workload's size.
const SYNTHETIC: usize = 16 << 20;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mux {
    Direct,
    Fux,
    Tmux,
    Zellij,
}

impl Mux {
    pub fn name(self) -> &'static str {
        match self {
            Mux::Direct => "direct",
            Mux::Fux => "fux",
            Mux::Tmux => "tmux",
            Mux::Zellij => "zellij",
        }
    }

    fn of(name: &str) -> Option<Mux> {
        [Mux::Direct, Mux::Fux, Mux::Tmux, Mux::Zellij]
            .into_iter()
            .find(|m| m.name() == name)
    }
}

pub struct Options {
    pub muxes: Vec<String>,
    pub parts: Vec<String>,
    pub keys: usize,
    pub json: PathBuf,
}

/// What every session needs: where to keep its files, this binary (the
/// pane programs), and the fux to run.
struct Place {
    dir: PathBuf,
    me: String,
    fux: String,
    count: std::cell::Cell<u32>,
}

impl Place {
    /// A directory of its own for the next session.
    fn next(&self, what: &str) -> Result<PathBuf, String> {
        let n = self.count.get().saturating_add(1);
        self.count.set(n);
        let dir = self.dir.join(format!("{what}{n}"));
        private(&dir)?;
        Ok(dir)
    }
}

/// Makes `dir`, readable by this user alone, as fux wants its socket's
/// directory.
fn private(dir: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
        .map_err(|e| format!("{}: {e}", dir.display()))
}

/// `program`, with none of `FOREIGN` in its environment: what it is to
/// talk to is set after this, explicitly.
fn clean(program: impl AsRef<std::ffi::OsStr>) -> Command {
    let mut command = Command::new(program);
    for name in FOREIGN {
        command.env_remove(name);
    }
    command
}

fn output(command: &mut Command) -> Result<String, String> {
    let out = command
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("{command:?}: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "{command:?} failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
}

/// A word quoted for `sh`.
fn quoted(word: &str) -> String {
    format!("'{}'", word.replace('\'', "'\\''"))
}

/// A multiplexer running with a client on a terminal; stopped when
/// dropped.
struct Session {
    mux: Mux,
    terminal: Terminal,
    /// fux's server, which this process started.
    server: Option<std::process::Child>,
    server_pid: Option<u32>,
    /// fux's socket, tmux's socket name, zellij's session name.
    handle: String,
    env: Vec<(String, String)>,
    dir: PathBuf,
}

impl Session {
    fn env(&self) -> Vec<(&str, &str)> {
        self.env
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect()
    }

    /// Runs the multiplexer's own command line against this session.
    fn command(&self, place: &Place, args: &[&str]) -> Result<String, String> {
        let mut command = match self.mux {
            Mux::Direct => return Err("no multiplexer".into()),
            Mux::Fux => {
                // Never the user's server: only the socket this session made.
                if !self.env.iter().any(|(k, _)| k == "FUX_SOCKET") {
                    return Err("no socket for fux".into());
                }
                let mut c = clean(&place.fux);
                c.args(args);
                c
            }
            Mux::Tmux => {
                let mut c = clean("tmux");
                c.args(["-L", &self.handle, "-f", "/dev/null"]).args(args);
                c
            }
            Mux::Zellij => {
                let mut c = clean("zellij");
                c.args(["--session", &self.handle]).args(args);
                c
            }
        };
        for (k, v) in self.env() {
            command.env(k, v);
        }
        output(&mut command)
    }

    /// A new pane in a tab of its own, running `program` (`None`: a shell).
    fn new_pane(&self, place: &Place, program: Option<&[String]>) -> Result<(), String> {
        match self.mux {
            Mux::Direct => Err("no multiplexer".into()),
            Mux::Fux => {
                let mut args = vec!["new-tab", "-t", "main"];
                if let Some(p) = program {
                    args.push("--");
                    args.extend(p.iter().map(String::as_str));
                }
                self.command(place, &args).map(drop)
            }
            Mux::Tmux => {
                let mut args = vec!["new-window", "-d"];
                let line = program.map(|p| {
                    let words: Vec<String> = p.iter().map(|w| quoted(w)).collect();
                    format!("exec {}", words.join(" "))
                });
                if let Some(line) = &line {
                    args.push(line);
                }
                self.command(place, &args).map(drop)
            }
            Mux::Zellij => match program {
                None => self.command(place, &["action", "new-tab"]).map(drop),
                Some(p) => {
                    let layout = self
                        .dir
                        .join(format!("tab-{}.kdl", place.count.get().saturating_add(1)));
                    place.count.set(place.count.get().saturating_add(1));
                    std::fs::write(&layout, zellij_layout(&[p])).map_err(|e| e.to_string())?;
                    let path = layout.to_string_lossy();
                    self.command(place, &["action", "new-tab", "--layout", &path])
                        .map(drop)
                }
            },
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let env: Vec<(String, String)> = self.env.clone();
        let run = |program: &str, args: &[&str]| {
            let mut c = clean(program);
            c.args(args)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null());
            for (k, v) in &env {
                c.env(k, v);
            }
            let _ = c.status();
        };
        match self.mux {
            Mux::Direct => {}
            Mux::Fux => {
                let socket = env.iter().any(|(k, _)| k == "FUX_SOCKET");
                if let Some(fux) = env.iter().find(|(k, _)| k == "FUX_BIN").map(|(_, v)| v)
                    && socket
                {
                    run(fux, &["kill-server"]);
                }
            }
            Mux::Tmux => run(
                "tmux",
                &["-L", &self.handle, "-f", "/dev/null", "kill-server"],
            ),
            Mux::Zellij => run("zellij", &["kill-session", &self.handle]),
        }
        if let Some(server) = &mut self.server {
            let deadline = Instant::now().checked_add(Duration::from_secs(2));
            while matches!(server.try_wait(), Ok(None))
                && deadline.is_some_and(|d| Instant::now() < d)
            {
                std::thread::sleep(Duration::from_millis(20));
            }
            let _ = server.kill();
            let _ = server.wait();
        }
        if let Some(pid) = self
            .server_pid
            .and_then(|p| i32::try_from(p).ok())
            .and_then(fuxix::process::Pid::from_raw)
            && self.mux == Mux::Zellij
        {
            let _ = fuxix::process::kill(pid, fuxix::process::Signal::Kill);
        }
    }
}

/// A zellij layout of the panes `programs` (the last focused), side by
/// side, or with no programs one shell.
fn zellij_layout(programs: &[&[String]]) -> String {
    let pane = |p: &[String], focus: bool| {
        let Some((program, args)) = p.split_first() else {
            return "pane".to_owned();
        };
        let args: Vec<String> = args
            .iter()
            .map(|a| format!("\"{}\"", a.replace('\\', "\\\\").replace('"', "\\\"")))
            .collect();
        format!(
            "pane command=\"{program}\"{} {{ args {}; }}",
            if focus { " focus=true" } else { "" },
            args.join(" ")
        )
    };
    let count = programs.len();
    let panes: Vec<String> = programs
        .iter()
        .enumerate()
        .map(|(i, p)| pane(p, i.saturating_add(1) == count))
        .collect();
    if panes.len() > 1 {
        format!(
            "layout {{\n    pane split_direction=\"vertical\" {{\n        {}\n    }}\n}}\n",
            panes.join("\n        ")
        )
    } else {
        format!(
            "layout {{\n    {}\n}}\n",
            panes.first().map_or("pane", String::as_str)
        )
    }
}

/// Starts `mux` with `programs` in its first tab, side by side (the last
/// focused; none: a shell), and a client on a terminal, watching for
/// `ready`.
fn start(
    place: &Place,
    mux: Mux,
    programs: &[Vec<String>],
    ready: Option<&[u8]>,
) -> Result<Session, String> {
    let dir = place.next(mux.name())?;
    let home = dir.to_string_lossy().into_owned();
    let mut env: Vec<(String, String)> = vec![
        ("HOME".into(), home.clone()),
        ("SHELL".into(), "/bin/sh".into()),
        ("ENV".into(), "/dev/null".into()),
        ("PS1".into(), "$ ".into()),
    ];
    let borrowed = |env: &[(String, String)]| -> Vec<(String, String)> { env.to_vec() };
    let line = |p: &[String]| -> String {
        let words: Vec<String> = p.iter().map(|w| quoted(w)).collect();
        format!("exec {}", words.join(" "))
    };
    match mux {
        Mux::Direct => {
            let first = programs.first().ok_or("no program")?;
            let (program, args) = first.split_first().ok_or("no program")?;
            let args: Vec<&str> = args.iter().map(String::as_str).collect();
            let env_ref: Vec<(&str, &str)> =
                env.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
            let terminal = Terminal::start(ROWS, COLS, program, &args, &env_ref, ready)?;
            Ok(Session {
                mux,
                terminal,
                server: None,
                server_pid: None,
                handle: String::new(),
                env,
                dir,
            })
        }
        Mux::Fux => {
            let socket = dir.join("s");
            let config = dir.join("fux.conf");
            std::fs::write(
                &config,
                format!("set shell /bin/sh\nset history-lines {HISTORY}\n"),
            )
            .map_err(|e| e.to_string())?;
            let log = std::fs::File::create(dir.join("server.log")).map_err(|e| e.to_string())?;
            let mut command = clean(&place.fux);
            command
                .arg("server")
                .arg("--socket")
                .arg(&socket)
                .arg("--config")
                .arg(&config)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(log);
            for (k, v) in &env {
                command.env(k, v);
            }
            let server = command.spawn().map_err(|e| format!("fux server: {e}"))?;
            let server_pid = server.id();
            let deadline = Instant::now().checked_add(READY_WAIT).ok_or("a deadline")?;
            while std::os::unix::net::UnixStream::connect(&socket).is_err() {
                if Instant::now() > deadline {
                    let log = std::fs::read_to_string(dir.join("server.log")).unwrap_or_default();
                    return Err(format!("the fux server did not start: {log}"));
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            let socket = socket.to_string_lossy().into_owned();
            env.push(("FUX_SOCKET".into(), socket.clone()));
            env.push(("FUX_BIN".into(), place.fux.clone()));
            let env_ref: Vec<(&str, &str)> =
                env.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
            let terminal = Terminal::start(
                ROWS.saturating_add(1),
                COLS,
                &place.fux,
                &["attach"],
                &env_ref,
                ready,
            )?;
            let session = Session {
                mux,
                terminal,
                server: Some(server),
                server_pid: Some(server_pid),
                handle: socket,
                env: borrowed(&env),
                dir,
            };
            for (i, p) in programs.iter().enumerate() {
                if i == 0 {
                    session.command(place, &["send-keys", "-t", "%1", &line(p), "Enter"])?;
                } else {
                    let mut args = vec!["split", "-h", "-t", "%1", "--"];
                    args.extend(p.iter().map(String::as_str));
                    session.command(place, &args)?;
                }
            }
            Ok(session)
        }
        Mux::Tmux => {
            let handle = format!("fux-bench-{}-{}", std::process::id(), place.count.get());
            let session = |terminal: Terminal, env: Vec<(String, String)>| Session {
                mux,
                terminal,
                server: None,
                server_pid: None,
                handle: handle.clone(),
                env,
                dir: dir.clone(),
            };
            let rows = ROWS.to_string();
            let cols = COLS.to_string();
            let mut tmux = clean("tmux");
            tmux.args([
                "-L",
                &handle,
                "-f",
                "/dev/null",
                "new-session",
                "-d",
                "-s",
                "bench",
            ])
            .args(["-x", &cols, "-y", &rows]);
            if let Some(first) = programs.first() {
                tmux.arg(line(first));
            }
            tmux.args([";", "set", "-g", "history-limit", &HISTORY.to_string()]);
            for p in programs.iter().skip(1) {
                tmux.args([";", "split-window", "-h", "-t", "bench"])
                    .arg(line(p));
            }
            for (k, v) in &env {
                tmux.env(k, v);
            }
            output(&mut tmux)?;
            let env_ref: Vec<(&str, &str)> =
                env.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
            let terminal = Terminal::start(
                ROWS.saturating_add(1),
                COLS,
                "tmux",
                &["-L", &handle, "-f", "/dev/null", "attach", "-t", "bench"],
                &env_ref,
                ready,
            )?;
            let mut s = session(terminal, env);
            s.server_pid = s
                .command(place, &["display", "-p", "#{pid}"])
                .ok()
                .and_then(|p| p.parse().ok());
            Ok(s)
        }
        Mux::Zellij => {
            let handle = format!("fux-bench-{}-{}", std::process::id(), place.count.get());
            let config_dir = dir.join("config");
            std::fs::create_dir_all(&config_dir).map_err(|e| e.to_string())?;
            let config = config_dir.join("config.kdl");
            std::fs::write(
                &config,
                format!(
                    "pane_frames false\nshow_startup_tips false\nshow_release_notes false\n\
                     default_shell \"/bin/sh\"\nscroll_buffer_size {HISTORY}\n\
                     session_serialization false\nmouse_mode false\n"
                ),
            )
            .map_err(|e| e.to_string())?;
            let layout = dir.join("layout.kdl");
            let refs: Vec<&[String]> = programs.iter().map(Vec::as_slice).collect();
            std::fs::write(&layout, zellij_layout(&refs)).map_err(|e| e.to_string())?;
            env.push((
                "ZELLIJ_SOCKET_DIR".into(),
                place.dir.join("zs").to_string_lossy().into_owned(),
            ));
            let env_ref: Vec<(&str, &str)> =
                env.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
            let config = config.to_string_lossy().into_owned();
            let config_dir = config_dir.to_string_lossy().into_owned();
            let data = dir.join("data").to_string_lossy().into_owned();
            let layout = layout.to_string_lossy().into_owned();
            let terminal = Terminal::start(
                ROWS.saturating_add(1),
                COLS,
                "zellij",
                &[
                    "--config",
                    &config,
                    "--config-dir",
                    &config_dir,
                    "--data-dir",
                    &data,
                    "--session",
                    &handle,
                    "--new-session-with-layout",
                    &layout,
                ],
                &env_ref,
                ready,
            )?;
            let mut s = Session {
                mux,
                terminal,
                server: None,
                server_pid: None,
                handle,
                env,
                dir,
            };
            // The server is a process of its own, found by its socket.
            let deadline = Instant::now().checked_add(READY_WAIT).ok_or("a deadline")?;
            while s.server_pid.is_none() && Instant::now() < deadline {
                s.server_pid = zellij_server(&s.handle);
                if s.server_pid.is_none() {
                    std::thread::sleep(Duration::from_millis(50));
                }
            }
            Ok(s)
        }
    }
}

/// zellij's server for the session `name`: `zellij --server …/name`.
fn zellij_server(name: &str) -> Option<u32> {
    let list = output(Command::new("ps").args(["-axo", "pid=,command="])).ok()?;
    list.lines().find_map(|line| {
        let (pid, command) = line.trim().split_once(' ')?;
        (command.contains("zellij") && command.contains("--server") && command.ends_with(name))
            .then(|| pid.parse().ok())
            .flatten()
    })
}

/// A process's resident memory in KiB and CPU time in seconds.
fn usage(pid: u32) -> Option<(u64, f64)> {
    let text =
        output(Command::new("ps").args(["-o", "rss=,time=", "-p", &pid.to_string()])).ok()?;
    let mut words = text.split_whitespace();
    let rss = words.next()?.parse().ok()?;
    // [[DD-]HH:]MM:SS[.cc]
    let time = words.next()?;
    let (days, clock) = time.split_once('-').unwrap_or(("0", time));
    let mut seconds = days.parse::<f64>().ok()? * 86_400.0;
    let mut scale = 1.0;
    for part in clock.rsplit(':') {
        seconds += part.parse::<f64>().ok()? * scale;
        scale *= 60.0;
    }
    Some((rss, seconds))
}

/// The median and the 99th percentile of `values`, in ms, and their mean.
fn summary(values: &mut [f64]) -> serde_json::Value {
    values.sort_by(f64::total_cmp);
    // The value `percent` of the way from the least to the most, nearest.
    let at = |percent: usize| -> f64 {
        let last = values.len().saturating_sub(1);
        let i = last
            .saturating_mul(percent)
            .saturating_add(50)
            .checked_div(100)
            .unwrap_or(0);
        values.get(i).copied().unwrap_or(f64::NAN)
    };
    let mean = values.iter().sum::<f64>() / (values.len().max(1) as f64);
    serde_json::json!({
        "keys": values.len(),
        "p50_ms": at(50),
        "p99_ms": at(99),
        "mean_ms": mean,
        "max_ms": values.last().copied().unwrap_or(f64::NAN),
    })
}

/// Latency: `keys` keys typed at `session`'s client, one at a time.
fn type_keys(session: &Session, keys: usize) -> Result<serde_json::Value, String> {
    let mut times = Vec::with_capacity(keys);
    let mut missed = 0usize;
    // The first keys whose echo never came, and the bytes the client was
    // sent meanwhile: whether it was painted at all.
    let mut misses = Vec::new();
    let mut in_a_row = 0usize;
    let (bytes_before, frames_before) = session.terminal.counts();
    let mut typed = 0usize;
    for i in 0..keys {
        if in_a_row >= MISSES_IN_A_ROW {
            break;
        }
        typed = typed.saturating_add(1);
        let (before, _) = session.terminal.counts();
        let key = b'a'.saturating_add(u8::try_from(i.checked_rem(26).unwrap_or(0)).unwrap_or(0));
        let glyph = helpers::circled(key).ok_or("a key")?;
        let mut buf = [0; 4];
        session
            .terminal
            .watch(glyph.encode_utf8(&mut buf).as_bytes());
        let sent = Instant::now();
        session.terminal.type_bytes(&[key])?;
        match session.terminal.arrival(KEY_WAIT) {
            Some(at) => {
                times.push(at.saturating_duration_since(sent).as_secs_f64() * 1e3);
                in_a_row = 0;
            }
            None => {
                missed = missed.saturating_add(1);
                in_a_row = in_a_row.saturating_add(1);
                if misses.len() < 10 {
                    let (after, _) = session.terminal.counts();
                    misses.push(serde_json::json!({
                        "key": i,
                        "client_bytes_meanwhile": after.saturating_sub(before),
                    }));
                }
            }
        }
        // 17 to 25 ms between keys: past fux's paint interval, as a person
        // types, but not in step with it.
        let gap = 17u64.saturating_add(
            u64::try_from(i.wrapping_mul(7).checked_rem(9).unwrap_or(0)).unwrap_or(0),
        );
        std::thread::sleep(Duration::from_millis(gap));
    }
    let (bytes_after, frames_after) = session.terminal.counts();
    let mut value = summary(&mut times);
    if let Some(map) = value.as_object_mut() {
        map.insert("missed".into(), serde_json::json!(missed));
        map.insert("misses".into(), serde_json::json!(misses));
        map.insert("typed".into(), serde_json::json!(typed));
        let typed = typed.max(1) as f64;
        map.insert(
            "client_bytes_per_key".into(),
            serde_json::json!((bytes_after.saturating_sub(bytes_before) as f64) / typed),
        );
        map.insert(
            "paints_per_key".into(),
            serde_json::json!((frames_after.saturating_sub(frames_before) as f64) / typed),
        );
    }
    Ok(value)
}

/// A throughput workload: its name and file.
struct Load {
    name: String,
    path: PathBuf,
    bytes: u64,
}

/// Throughput: each of `loads` through `session`'s `__serve`.
fn serve_loads(session: &Session, loads: &[Load]) -> Result<serde_json::Value, String> {
    let mut out = serde_json::Map::new();
    for (i, load) in loads.iter().enumerate() {
        session.terminal.watch(helpers::marker(i).as_bytes());
        let (bytes_before, frames_before) = session.terminal.counts();
        let sent = Instant::now();
        session.terminal.type_bytes(format!("{i}\r").as_bytes())?;
        let arrived = session.terminal.arrival(LOAD_WAIT);
        std::thread::sleep(Duration::from_millis(100));
        let (bytes_after, frames_after) = session.terminal.counts();
        let seconds = arrived.map(|at| at.saturating_duration_since(sent).as_secs_f64());
        let client = bytes_after.saturating_sub(bytes_before);
        let frames = frames_after.saturating_sub(frames_before);
        out.insert(
            load.name.clone(),
            serde_json::json!({
                "bytes": load.bytes,
                "seconds": seconds,
                "mb_per_s": seconds.map(|s| (load.bytes as f64) / s.max(1e-9) / 1e6),
                "client_bytes": client,
                "paints": (frames > 0).then_some(frames),
                "client_bytes_per_paint": (frames > 0).then(|| (client as f64) / (frames as f64)),
            }),
        );
        if arrived.is_none() {
            eprintln!(
                "  {}: {} did not arrive in {LOAD_WAIT:?}",
                session.mux.name(),
                load.name
            );
        }
    }
    Ok(serde_json::Value::Object(out))
}

/// Waits up to `limit` for every file in `files` to exist.
fn wait_files(files: &[PathBuf], limit: Duration) -> bool {
    let deadline = Instant::now().checked_add(limit);
    while !files.iter().all(|f| f.exists()) {
        if deadline.is_none_or(|d| Instant::now() > d) {
            return false;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    true
}

/// Memory: the server's resident memory now, with 4 more panes, and with 4
/// more of full history.
fn memory(place: &Place, session: &Session) -> Result<serde_json::Value, String> {
    let pid = session.server_pid.ok_or("no server process")?;
    let rss = || usage(pid).map(|(rss, _)| rss).unwrap_or(0);
    let start = rss();
    let stage = |rows: u64| -> Result<u64, String> {
        let mut done = Vec::new();
        for _ in 0..4 {
            let file = session.dir.join(format!("done-{}", place.count.get()));
            place.count.set(place.count.get().saturating_add(1));
            let program = vec![
                place.me.clone(),
                "__fill".into(),
                rows.to_string(),
                file.to_string_lossy().into_owned(),
            ];
            session.new_pane(place, Some(&program))?;
            done.push(file);
        }
        if !wait_files(&done, READY_WAIT) {
            return Err("the panes did not fill".into());
        }
        // What the programs wrote last may still be on its way.
        std::thread::sleep(Duration::from_secs(1));
        Ok(rss())
    };
    let empty = stage(0)?;
    let full = stage(HISTORY.saturating_add(50))?;
    let per = |a: u64, b: u64| (b.saturating_sub(a) as f64) / 4.0;
    Ok(serde_json::json!({
        "server_kib_at_start": start,
        "kib_per_empty_pane": per(start, empty),
        "kib_per_pane_with_full_history": per(empty, full),
        "client_kib": usage(session.terminal.pid()).map(|(rss, _)| rss),
    }))
}

/// The server's resident memory with 50 panes, each a shell at its prompt,
/// in a session of its own.
fn fifty(place: &Place, mux: Mux) -> Result<u64, String> {
    let session = start(place, mux, &[], None)?;
    for _ in 1..50 {
        session.new_pane(place, None)?;
    }
    std::thread::sleep(Duration::from_secs(2));
    let pid = session.server_pid.ok_or("no server process")?;
    usage(pid).map(|(rss, _)| rss).ok_or("no memory".into())
}

pub fn run(options: &Options) -> Result<bool, String> {
    if cfg!(debug_assertions) {
        return Err("a debug build: run it with --release".into());
    }
    let started = Instant::now();
    let root = crate::against::root()
        .canonicalize()
        .map_err(|e| e.to_string())?;
    let mut muxes = Vec::new();
    for name in &options.muxes {
        let mux = Mux::of(name).ok_or(format!("no multiplexer {name:?}"))?;
        let installed = match mux {
            Mux::Direct | Mux::Fux => true,
            Mux::Tmux | Mux::Zellij => {
                output(Command::new("sh").args(["-c", &format!("command -v {}", mux.name())]))
                    .is_ok()
            }
        };
        if installed {
            muxes.push(mux);
        } else {
            eprintln!("{name} is not installed: skipped");
        }
    }
    let part = |p: &str| options.parts.iter().any(|q| q == p);
    eprintln!("building fux…");
    output(
        Command::new("cargo")
            .args(["build", "--release", "--quiet", "--bin", "fux"])
            .current_dir(&root),
    )?;
    let fux = root
        .join("target/release/fux")
        .to_string_lossy()
        .into_owned();
    let mut versions = serde_json::Map::new();
    for mux in &muxes {
        let version = match mux {
            Mux::Direct => Ok(String::new()),
            Mux::Fux => output(Command::new(&fux).arg("--version")),
            Mux::Tmux => output(Command::new("tmux").arg("-V")),
            Mux::Zellij => output(Command::new("zellij").arg("--version")),
        };
        versions.insert(
            mux.name().into(),
            serde_json::json!(version.unwrap_or_default()),
        );
    }
    // Short: fux's socket goes in it.
    let dir = PathBuf::from(format!("/tmp/fux-feel-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    private(&dir)?;
    let place = Place {
        dir: dir.clone(),
        me: std::env::current_exe()
            .map_err(|e| e.to_string())?
            .to_string_lossy()
            .into_owned(),
        fux,
        count: std::cell::Cell::new(0),
    };
    let result = measure(&place, &root, &muxes, options, &part);
    let _ = std::fs::remove_dir_all(&dir);
    let (latency, throughput, footprint) = result?;
    let total = started.elapsed().as_secs_f64();
    let json = serde_json::json!({
        "kind": "fux-bench feel",
        "versions": versions,
        "pane": [ROWS, COLS],
        "latency": latency,
        "throughput": throughput,
        "footprint": footprint,
        "seconds": total,
    });
    print!("{}", report(&json, &muxes));
    println!("{total:.0} s in all");
    crate::save_json(&options.json, &json)?;
    eprintln!("wrote {}", options.json.display());
    Ok(true)
}

type Parts = (serde_json::Value, serde_json::Value, serde_json::Value);

fn measure(
    place: &Place,
    root: &Path,
    muxes: &[Mux],
    options: &Options,
    part: &dyn Fn(&str) -> bool,
) -> Result<Parts, String> {
    let mut latency = serde_json::Map::new();
    let echo = vec![place.me.clone(), "__echo".into()];
    let flood = vec![place.me.clone(), "__flood".into()];
    if part("latency") {
        for &mux in muxes {
            let mut by = serde_json::Map::new();
            let setups: &[(&str, Vec<Vec<String>>)] = &[
                ("idle", vec![echo.clone()]),
                ("flood", vec![flood.clone(), echo.clone()]),
            ];
            for (what, programs) in setups {
                if mux == Mux::Direct && *what == "flood" {
                    continue;
                }
                let t = Instant::now();
                let session = start(place, mux, programs, Some(helpers::READY.as_bytes()))?;
                if session.terminal.arrival(READY_WAIT).is_none() {
                    return Err(format!("{}: the echo program never showed", mux.name()));
                }
                std::thread::sleep(Duration::from_millis(300));
                by.insert((*what).into(), type_keys(&session, options.keys)?);
                eprintln!(
                    "latency {} {what}: {:.0} s",
                    mux.name(),
                    t.elapsed().as_secs_f64()
                );
            }
            latency.insert(mux.name().into(), serde_json::Value::Object(by));
        }
    }
    let mut throughput = serde_json::Map::new();
    if part("throughput") {
        let loads = loads(place, root)?;
        let list = place.dir.join("loads");
        let paths: Vec<String> = loads
            .iter()
            .map(|l| l.path.to_string_lossy().into_owned())
            .collect();
        std::fs::write(&list, paths.join("\n")).map_err(|e| e.to_string())?;
        let serve = vec![
            place.me.clone(),
            "__serve".into(),
            list.to_string_lossy().into_owned(),
        ];
        for &mux in muxes {
            let t = Instant::now();
            let session = start(
                place,
                mux,
                std::slice::from_ref(&serve),
                Some(helpers::READY.as_bytes()),
            )?;
            if session.terminal.arrival(READY_WAIT).is_none() {
                return Err(format!("{}: the serving program never showed", mux.name()));
            }
            std::thread::sleep(Duration::from_millis(300));
            throughput.insert(mux.name().into(), serve_loads(&session, &loads)?);
            eprintln!(
                "throughput {}: {:.0} s",
                mux.name(),
                t.elapsed().as_secs_f64()
            );
        }
    }
    let mut footprint = serde_json::Map::new();
    if part("footprint") {
        let t = Instant::now();
        let sessions: Vec<Session> = muxes
            .iter()
            .filter(|m| **m != Mux::Direct)
            .map(|&mux| start(place, mux, &[], None))
            .collect::<Result<_, _>>()?;
        std::thread::sleep(Duration::from_secs(2));
        let cpu = |s: &Session| -> f64 {
            [s.server_pid, Some(s.terminal.pid())]
                .into_iter()
                .flatten()
                .filter_map(usage)
                .map(|(_, cpu)| cpu)
                .sum()
        };
        let before: Vec<f64> = sessions.iter().map(cpu).collect();
        std::thread::sleep(Duration::from_secs(10));
        let after: Vec<f64> = sessions.iter().map(cpu).collect();
        for ((s, b), a) in sessions.iter().zip(&before).zip(&after) {
            let mut value = memory(place, s)?;
            if let Some(map) = value.as_object_mut() {
                map.insert("idle_cpu_seconds_in_10s".into(), serde_json::json!(a - b));
            }
            footprint.insert(s.mux.name().into(), value);
        }
        drop(sessions);
        for &mux in muxes.iter().filter(|m| **m != Mux::Direct) {
            let kib = fifty(place, mux)?;
            if let Some(map) = footprint
                .get_mut(mux.name())
                .and_then(serde_json::Value::as_object_mut)
            {
                map.insert("server_kib_with_50_panes".into(), serde_json::json!(kib));
            }
        }
        eprintln!("footprint: {:.0} s", t.elapsed().as_secs_f64());
    }
    Ok((
        serde_json::Value::Object(latency),
        serde_json::Value::Object(throughput),
        serde_json::Value::Object(footprint),
    ))
}

/// The workloads: every recording, and each synthetic workload, 16 MiB, in
/// files.
fn loads(place: &Place, root: &Path) -> Result<Vec<Load>, String> {
    let mut out = Vec::new();
    let corpus = crate::corpus::dir(root);
    for r in crate::corpus::recordings(&corpus)? {
        let path = corpus.join(format!("{}.bin", r.name));
        let bytes = std::fs::metadata(&path).map_err(|e| e.to_string())?.len();
        out.push(Load {
            name: format!("corpus:{}", r.name),
            path,
            bytes,
        });
    }
    for (name, _, _) in crate::synthetic::GENERATORS {
        let bytes = crate::synthetic::make(name, SYNTHETIC).unwrap_or_default();
        let path = place.dir.join(format!("{name}.bin"));
        std::fs::write(&path, &bytes).map_err(|e| e.to_string())?;
        out.push(Load {
            name: (*name).to_owned(),
            path,
            bytes: u64::try_from(bytes.len()).unwrap_or(0),
        });
    }
    Ok(out)
}

/// The human summary of `json`.
fn report(json: &serde_json::Value, muxes: &[Mux]) -> String {
    let mut out = String::new();
    let get = |path: &[&str]| -> Option<&serde_json::Value> {
        path.iter().try_fold(json, |v, key| v.get(key))
    };
    let number = |path: &[&str]| get(path).and_then(serde_json::Value::as_f64);
    let cell = |v: Option<f64>, digits: usize| {
        v.map_or_else(|| "-".to_owned(), |n| format!("{n:.digits$}"))
    };
    if get(&["latency"]).is_some_and(|l| l.as_object().is_some_and(|m| !m.is_empty())) {
        let _ = writeln!(
            out,
            "keystroke latency, ms (key at the client's PTY to its echo painted)\n{:<8} {:>9} {:>9} {:>10} {:>10} {:>11}",
            "", "idle p50", "idle p99", "flood p50", "flood p99", "bytes/key"
        );
        for mux in muxes {
            let m = mux.name();
            let _ = writeln!(
                out,
                "{m:<8} {:>9} {:>9} {:>10} {:>10} {:>11}",
                cell(number(&["latency", m, "idle", "p50_ms"]), 2),
                cell(number(&["latency", m, "idle", "p99_ms"]), 2),
                cell(number(&["latency", m, "flood", "p50_ms"]), 2),
                cell(number(&["latency", m, "flood", "p99_ms"]), 2),
                cell(number(&["latency", m, "idle", "client_bytes_per_key"]), 0),
            );
        }
        out.push('\n');
    }
    if let Some(serde_json::Value::Object(first)) =
        muxes.first().and_then(|m| get(&["throughput", m.name()]))
    {
        let head = |out: &mut String, title: &str| {
            let _ = write!(out, "{title}\n{:<24}", "");
            for mux in muxes {
                let _ = write!(out, " {:>17}", mux.name());
            }
            out.push('\n');
        };
        head(
            &mut out,
            "throughput, 16 MiB to the final screen: MB/s (MB sent to the client)",
        );
        for load in first.keys().filter(|l| !l.starts_with("corpus:")) {
            let _ = write!(out, "{load:<24}");
            for mux in muxes {
                let m = mux.name();
                let mbs = number(&["throughput", m, load, "mb_per_s"]);
                let sent = number(&["throughput", m, load, "client_bytes"]).map(|b| b / 1e6);
                let _ = write!(out, " {:>8} ({:>6})", cell(mbs, 1), cell(sent, 2));
            }
            out.push('\n');
        }
        out.push('\n');
        head(
            &mut out,
            "each recording to the final screen: ms (KB sent to the client)",
        );
        let mut totals = vec![(0f64, 0f64); muxes.len()];
        for load in first.keys().filter(|l| l.starts_with("corpus:")) {
            let _ = write!(out, "{load:<24}");
            for (mux, total) in muxes.iter().zip(&mut totals) {
                let m = mux.name();
                let ms = number(&["throughput", m, load, "seconds"]).map(|s| s * 1e3);
                let sent = number(&["throughput", m, load, "client_bytes"]).map(|b| b / 1e3);
                total.0 += ms.unwrap_or(f64::NAN);
                total.1 += sent.unwrap_or(0.0);
                let _ = write!(out, " {:>8} ({:>6})", cell(ms, 1), cell(sent, 1));
            }
            out.push('\n');
        }
        let _ = write!(out, "{:<24}", "all recordings");
        for (ms, sent) in &totals {
            let _ = write!(
                out,
                " {:>8} ({:>6})",
                cell(Some(*ms), 1),
                cell(Some(*sent), 1)
            );
        }
        out.push_str("\n\n");
    }
    if get(&["footprint"]).is_some_and(|l| l.as_object().is_some_and(|m| !m.is_empty())) {
        let _ = writeln!(
            out,
            "footprint\n{:<8} {:>12} {:>12} {:>14} {:>13} {:>14}",
            "", "server KiB", "KiB/pane", "KiB/full pane", "50 panes KiB", "idle CPU s/10s"
        );
        for mux in muxes.iter().filter(|m| **m != Mux::Direct) {
            let m = mux.name();
            let _ = writeln!(
                out,
                "{m:<8} {:>12} {:>12} {:>14} {:>13} {:>14}",
                cell(number(&["footprint", m, "server_kib_at_start"]), 0),
                cell(number(&["footprint", m, "kib_per_empty_pane"]), 0),
                cell(
                    number(&["footprint", m, "kib_per_pane_with_full_history"]),
                    0
                ),
                cell(number(&["footprint", m, "server_kib_with_50_panes"]), 0),
                cell(number(&["footprint", m, "idle_cpu_seconds_in_10s"]), 2),
            );
        }
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_summary_has_its_percentiles() {
        let mut values: Vec<f64> = (1..=100).map(f64::from).collect();
        let s = super::summary(&mut values);
        assert_eq!(
            s.get("p50_ms").and_then(serde_json::Value::as_f64),
            Some(51.0)
        );
        assert_eq!(
            s.get("p99_ms").and_then(serde_json::Value::as_f64),
            Some(99.0)
        );
        assert_eq!(
            s.get("max_ms").and_then(serde_json::Value::as_f64),
            Some(100.0)
        );
    }

    #[test]
    fn a_zellij_layout_focuses_its_last_pane() {
        let flood = vec!["/b".to_owned(), "__flood".to_owned()];
        let echo = vec!["/b".to_owned(), "__echo".to_owned()];
        let text = super::zellij_layout(&[&flood, &echo]);
        assert!(text.contains("split_direction=\"vertical\""));
        assert!(text.contains("pane command=\"/b\" { args \"__flood\"; }"));
        assert!(text.contains("pane command=\"/b\" focus=true { args \"__echo\"; }"));
        assert_eq!(super::zellij_layout(&[]), "layout {\n    pane\n}\n");
    }

    #[test]
    fn words_are_quoted_for_sh() {
        assert_eq!(super::quoted("a b"), "'a b'");
        assert_eq!(super::quoted("it's"), "'it'\\''s'");
    }
}
