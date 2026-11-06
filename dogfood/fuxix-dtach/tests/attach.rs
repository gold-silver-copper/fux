//! Starts a program detached, attaches a terminal (a PTY this test holds
//! the master of), types into it, detaches, attaches again at another size,
//! and ends the program.
use fuxix::poll::{Events, PollFd, poll};
use fuxix::terminal::Size;
use std::process::{Child, Command};
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_fuxix-dtach");

type Result<T = ()> = std::result::Result<T, String>;

/// A terminal of `rows` by `cols` with `fuxix-dtach ARGS` attached on it.
struct Terminal {
    master: fuxix::pty::Master,
    child: Child,
    seen: Vec<u8>,
}

impl Terminal {
    fn attach(socket: &str, rows: u16, cols: u16) -> Result<Terminal> {
        let size = Size::new(rows, cols).ok_or("size")?;
        let (master, slave) = fuxix::pty::open(size).map_err(|e| e.to_string())?;
        let (child, _) = slave
            .spawn(&[BIN, "-a", socket], fuxix::process::Dispositions::Default, |_| {})
            .map_err(|e| e.to_string())?;
        Ok(Terminal {
            master,
            child,
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

    fn exits_ok(mut self) -> Result {
        let status = self.child.wait().map_err(|e| e.to_string())?;
        status.success().then_some(()).ok_or(format!("{status}"))
    }
}

#[test]
fn a_program_outlives_its_terminals_and_sees_each_ones_size() -> Result {
    let socket = std::env::temp_dir().join(format!("dtach-{}.sock", std::process::id()));
    let socket = socket.to_str().ok_or("path")?.to_owned();
    let script = "while read l; do echo \"got:$l\"; stty size; done";
    let started = Command::new(BIN)
        .args(["-n", &socket, "sh", "-c", script])
        .status()
        .map_err(|e| e.to_string())?;
    assert!(started.success(), "-n: {started}");
    // A second start on a live socket is refused.
    let again = Command::new(BIN)
        .args(["-n", &socket, "true"])
        .output()
        .map_err(|e| e.to_string())?;
    assert!(!again.status.success());

    let mut first = Terminal::attach(&socket, 30, 100)?;
    first.type_in(b"hello\r")?;
    first.wait_for("got:hello")?;
    first.wait_for("30 100")?;
    // The terminal resized while attached: the program sees it.
    let resized = Size::new(25, 90).ok_or("size")?;
    fuxix::terminal::set_window_size(&first.master, resized, None)
        .map_err(|e| e.to_string())?;
    std::thread::sleep(Duration::from_millis(200));
    first.type_in(b"x\r")?;
    first.wait_for("25 90")?;
    first.type_in(b"\x1c")?;
    first.wait_for("[detached]")?;
    first.exits_ok()?;

    let mut second = Terminal::attach(&socket, 10, 40)?;
    second.type_in(b"again\r")?;
    second.wait_for("got:again")?;
    second.wait_for("10 40")?;
    // End of input ends the loop, the program, and the server.
    second.type_in(b"\x04")?;
    second.wait_for("[EOF - fuxix-dtach terminating]")?;
    second.exits_ok()?;
    let gone = Instant::now() + Duration::from_secs(5);
    while std::path::Path::new(&socket).exists() && Instant::now() < gone {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(!std::path::Path::new(&socket).exists(), "the socket is removed");
    Ok(())
}
