//! The real viewer binary (`fux attach`) on a pseudo-terminal against a real `fux serve` on
//! disposable XDG directories (prompt section 5, "viewer tests against a pseudo-terminal"):
//! `SIGWINCH` re-lays out promptly and the server's viewport follows, a panic inside a viewer
//! system restores the outer terminal and exits non-zero, and a chooser dismisses on focus
//! loss. Skips with a message when no pseudo-terminal can be opened.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::print_stderr,
    reason = "integration-test helpers; clippy.toml only relaxes #[test] bodies"
)]

use std::cell::RefCell;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use portable_pty::{CommandBuilder, PtyPair, PtySize, native_pty_system};
use serde_json::json;

const WAIT: Duration = Duration::from_secs(20);

fn fux_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_fux"))
}

/// Private XDG directories for one test.
struct Xdg {
    _dir: tempfile::TempDir,
    runtime: PathBuf,
    config: PathBuf,
    state: PathBuf,
    home: PathBuf,
}

impl Xdg {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().to_path_buf();
        let runtime = home.join("run");
        let config = home.join("config");
        let state = home.join("state");
        for d in [&runtime, &config, &state] {
            std::fs::create_dir_all(d).unwrap();
        }
        Self {
            _dir: dir,
            runtime,
            config,
            state,
            home,
        }
    }

    fn apply(&self, cmd: &mut CommandBuilder) {
        cmd.env("XDG_RUNTIME_DIR", &self.runtime);
        cmd.env("XDG_CONFIG_HOME", &self.config);
        cmd.env("XDG_STATE_HOME", &self.state);
        cmd.env("HOME", &self.home);
        cmd.env("TERM", "xterm-256color");
    }
}

/// A `fux serve` on the test's XDG directories, terminated with the test.
struct Fux {
    child: Child,
    brp: PathBuf,
}

impl Fux {
    fn start(xdg: &Xdg) -> Self {
        let child = Command::new(fux_binary())
            .args(["serve", "--name", "vt"])
            .env("XDG_RUNTIME_DIR", &xdg.runtime)
            .env("XDG_CONFIG_HOME", &xdg.config)
            .env("XDG_STATE_HOME", &xdg.state)
            .env("HOME", &xdg.home)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let brp = xdg.runtime.join("fux").join("vt.brp.json");
        let deadline = Instant::now() + WAIT;
        while fux::remote::client::read_descriptor(&brp).is_err() {
            assert!(
                Instant::now() < deadline,
                "fux never published {}",
                brp.display()
            );
            std::thread::sleep(Duration::from_millis(50));
        }
        Self { child, brp }
    }

    fn call(&self, method: &str, params: serde_json::Value) -> serde_json::Value {
        fux::remote::client::call(&self.brp, method, params).unwrap()
    }
}

impl Drop for Fux {
    fn drop(&mut self) {
        let pid = nix::unistd::Pid::from_raw(i32::try_from(self.child.id()).unwrap());
        let _ = nix::sys::signal::kill(pid, nix::sys::signal::Signal::SIGTERM);
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if matches!(self.child.try_wait(), Ok(Some(_))) {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// The viewer on a pseudo-terminal: everything it wrote so far, and a screen parsed from it.
struct Viewer {
    pair: PtyPair,
    child: Box<dyn portable_pty::Child + Send + Sync>,
    writer: Box<dyn Write + Send>,
    chunks: Receiver<Vec<u8>>,
    output: RefCell<Vec<u8>>,
}

impl Viewer {
    /// `None` when no pseudo-terminal can be opened.
    fn attach(xdg: &Xdg, fux: &Fux, rows: u16, cols: u16, env: &[(&str, &str)]) -> Option<Self> {
        let pair = native_pty_system()
            .openpty(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .ok()?;
        let mut cmd = CommandBuilder::new(fux_binary());
        cmd.args([
            "attach",
            "--brp",
            fux.brp.to_str().unwrap(),
            "--workspace",
            "default",
        ]);
        xdg.apply(&mut cmd);
        for (k, v) in env {
            cmd.env(k, v);
        }
        let child = pair.slave.spawn_command(cmd).ok()?;
        let mut reader = pair.master.try_clone_reader().ok()?;
        let writer = pair.master.take_writer().ok()?;
        let (tx, chunks) = mpsc::channel();
        std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if tx.send(buf[..n].to_vec()).is_err() {
                            break;
                        }
                    }
                }
            }
        });
        Some(Self {
            pair,
            child,
            writer,
            chunks,
            output: RefCell::new(Vec::new()),
        })
    }

    /// Everything the viewer has written so far.
    fn output(&self) -> Vec<u8> {
        let mut output = self.output.borrow_mut();
        while let Ok(chunk) = self.chunks.try_recv() {
            output.extend_from_slice(&chunk);
        }
        output.clone()
    }

    /// The screen as a terminal of `rows`x`cols` would show it after everything written so far.
    fn screen(&self, rows: u16, cols: u16) -> Vec<String> {
        let mut parser = vt100::Parser::new(rows, cols, 0);
        parser.process(&self.output());
        parser
            .screen()
            .rows(0, cols)
            .map(|r| r.trim_end().to_owned())
            .collect()
    }

    fn wait_for(&self, what: &str, mut done: impl FnMut(&Self) -> bool) {
        let deadline = Instant::now() + WAIT;
        while !done(self) {
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {what}; output so far:\n{}",
                String::from_utf8_lossy(&self.output())
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn send(&mut self, bytes: &[u8]) {
        self.writer.write_all(bytes).unwrap();
        self.writer.flush().unwrap();
    }

    fn resize(&self, rows: u16, cols: u16) {
        self.pair
            .master
            .resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .unwrap();
    }
}

impl Drop for Viewer {
    fn drop(&mut self) {
        if matches!(self.child.try_wait(), Ok(None)) {
            // `prefix d` detaches; a stuck viewer is killed.
            let _ = self.writer.write_all(b"\x02d");
            let _ = self.writer.flush();
            let deadline = Instant::now() + Duration::from_secs(3);
            while Instant::now() < deadline {
                if matches!(self.child.try_wait(), Ok(Some(_))) {
                    return;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

fn contains(bytes: &[u8], needle: &[u8]) -> bool {
    bytes.windows(needle.len()).any(|w| w == needle)
}

fn bar_row(screen: &[String]) -> Option<usize> {
    screen.iter().position(|row| row.contains("1:main"))
}

#[test]
fn sigwinch_relays_out_and_the_server_viewport_follows() {
    let xdg = Xdg::new();
    let fux = Fux::start(&xdg);
    let Some(viewer) = Viewer::attach(&xdg, &fux, 24, 80, &[]) else {
        eprintln!("skipping: no pseudo-terminal could be opened");
        return;
    };
    // The bar is painted on the last row of an 80x24 terminal.
    viewer.wait_for("the status bar", |v| bar_row(&v.screen(24, 80)) == Some(23));
    fux.call("fux/viewer.list", json!({}));
    let viewers = fux.call("fux/viewer.list", json!({}));
    assert_eq!(
        viewers["viewers"][0]["rows"], 23,
        "pane area is one row short"
    );
    assert_eq!(viewers["viewers"][0]["cols"], 80);

    // TIOCSWINSZ on the master sends SIGWINCH: one update later the bar is on row 30 and row
    // 24 was cleared by the full repaint.
    viewer.resize(30, 100);
    viewer.wait_for("the bar on the new last row", |v| {
        let screen = v.screen(30, 100);
        bar_row(&screen) == Some(29) && !screen[23].contains("1:main")
    });
    viewer.wait_for("the server's viewport", |_| {
        let viewers = fux.call("fux/viewer.list", json!({}));
        viewers["viewers"][0]["rows"] == 29 && viewers["viewers"][0]["cols"] == 100
    });
}

#[test]
fn a_panic_inside_a_system_restores_the_terminal_and_exits_non_zero() {
    let xdg = Xdg::new();
    let fux = Fux::start(&xdg);
    let Some(mut viewer) = Viewer::attach(&xdg, &fux, 24, 80, &[("FUX_VIEWER_PANIC_AT", "4")])
    else {
        eprintln!("skipping: no pseudo-terminal could be opened");
        return;
    };
    viewer.wait_for("the status bar", |v| bar_row(&v.screen(24, 80)) == Some(23));
    // The runner only updates on wakes: keystrokes drive it to the panicking update.
    for _ in 0..4 {
        // The viewer may already be gone: a failed write is fine here.
        let _ = viewer
            .writer
            .write_all(b"x")
            .and_then(|()| viewer.writer.flush());
        std::thread::sleep(Duration::from_millis(50));
    }
    viewer.wait_for("the terminal restore sequence", |v| {
        let out = v.output();
        contains(&out, b"\x1b[?1049l") && contains(&out, b"\x1b[?25h")
    });
    let out = viewer.output();
    let enter = out
        .windows(8)
        .position(|w| w == b"\x1b[?1049h")
        .expect("the viewer entered the alternate screen first");
    let leave = out.windows(8).position(|w| w == b"\x1b[?1049l").unwrap();
    assert!(enter < leave, "entered before it restored");
    let status = viewer.child.wait().unwrap();
    assert!(
        !status.success(),
        "a panicking viewer exits non-zero: {status:?}"
    );
    assert!(
        contains(&out, b"FUX_VIEWER_PANIC_AT"),
        "the panic message reaches the terminal after the restore"
    );
}

#[test]
fn a_chooser_dismisses_when_a_click_takes_focus() {
    let xdg = Xdg::new();
    let fux = Fux::start(&xdg);
    let Some(mut viewer) = Viewer::attach(&xdg, &fux, 24, 80, &[]) else {
        eprintln!("skipping: no pseudo-terminal could be opened");
        return;
    };
    viewer.wait_for("the status bar", |v| bar_row(&v.screen(24, 80)) == Some(23));
    // `prefix w` opens the tab chooser above the tab strip.
    viewer.send(b"\x02w");
    viewer.wait_for("the tab chooser", |v| {
        let screen = v.screen(24, 80);
        screen[21].contains("tabs") && screen[22].contains("1:main")
    });
    // An SGR mouse press and release on a pane cell moves focus to the pane: the chooser is
    // gone with the next paint.
    viewer.send(b"\x1b[<0;40;5M\x1b[<0;40;5m");
    viewer.wait_for("the chooser to close", |v| {
        let screen = v.screen(24, 80);
        !screen[21].contains("tabs") && bar_row(&screen) == Some(23)
    });
    let status = viewer.child.try_wait().unwrap();
    assert!(status.is_none(), "the viewer keeps running");
}
