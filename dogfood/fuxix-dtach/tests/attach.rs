//! Starts a program detached, attaches a terminal (a PTY this test holds
//! the master of), types into it, detaches, attaches again at another size,
//! and ends the program.
use fuxix::poll::{Events, PollFd, poll};
use fuxix::terminal::Size;
use std::process::Command;
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_fuxix-dtach");

type Result<T = ()> = std::result::Result<T, String>;

/// A terminal of `rows` by `cols` with `fuxix-dtach ARGS` attached on it.
struct Terminal {
    master: fuxix::pty::Master,
    job: fuxix::process::Job,
    seen: Vec<u8>,
}

impl Terminal {
    fn attach(socket: &str, rows: u16, cols: u16) -> Result<Terminal> {
        let size = Size::new(rows, cols).ok_or("size")?;
        let _ = fuxix::pty::load(std::path::Path::new("no-pinned-conpty"));
        let (master, slave) = fuxix::pty::open(size).map_err(|e| e.to_string())?;
        let job = slave
            .spawn(&[BIN, "-a", socket], |_| {}, fuxix::process::Dispositions::Default)
            .map_err(|e| e.to_string())?;
        Ok(Terminal {
            master,
            job,
            seen: Vec::new(),
        })
    }

    fn type_in(&self, bytes: &[u8]) -> Result {
        let n = fuxix::io::write(&self.master, bytes).map_err(|e| e.to_string())?;
        (n == bytes.len()).then_some(()).ok_or("short write".into())
    }

    /// Reads until the screen has shown `text`, for 10 s at most.
    fn wait_for(&mut self, text: &str) -> Result {
        let end = Instant::now() + Duration::from_secs(10);
        let mut buffer = [0u8; 4096];
        while !String::from_utf8_lossy(&self.seen).contains(text) {
            let left = end.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(format!(
                    "no {text:?} in {:?}",
                    String::from_utf8_lossy(&self.seen)
                ));
            }
            let mut fds = [PollFd::new(&self.master, Events::IN)];
            let _ = poll(&mut fds, Some(left));
            match fuxix::io::read(&self.master, &mut buffer) {
                Ok(n) => self.seen.extend_from_slice(buffer.get(..n).unwrap_or_default()),
                Err(fuxix::Errno::AGAIN | fuxix::Errno::INTR) => {}
                Err(e) => return Err(format!("{e} before {text:?}")),
            }
        }
        Ok(())
    }

    fn ended(mut self) -> Result {
        let end = Instant::now() + Duration::from_secs(10);
        let mut buffer = [0u8; 4096];
        while self.job.ended().map_err(|e| e.to_string())?.is_none() {
            if Instant::now() > end {
                return Err("the terminal's client did not end".to_owned());
            }
            let mut fds = [PollFd::new(&self.master, Events::IN)];
            let _ = poll(&mut fds, Some(Duration::from_millis(50)));
            let _ = fuxix::io::read(&self.master, &mut buffer);
        }
        self.seen.clear();
        Ok(())
    }
}

#[test]
fn a_program_outlives_its_terminals() -> Result {
    let dir = std::env::temp_dir().join(format!("dtach-{}", std::process::id()));
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let socket = dir.join("s").to_string_lossy().into_owned();
    let started = Command::new(BIN)
        .args(["-n", &socket, "cmd.exe"])
        .status()
        .map_err(|e| e.to_string())?;
    assert!(started.success());
    let mut first = Terminal::attach(&socket, 24, 80)?;
    first.type_in(b"echo fir^st-marker\r")?;
    first.wait_for("first-marker")?;
    // Ctrl-\ detaches; the program runs on.
    first.type_in(b"\x1c")?;
    first.ended()?;
    let mut second = Terminal::attach(&socket, 30, 100)?;
    second.type_in(b"echo sec^ond-marker\r")?;
    second.wait_for("second-marker")?;
    second.type_in(b"exit\r")?;
    second.ended()
}
