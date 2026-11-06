//! The editor on a real PTY, under `sh`, whose modes are read once the
//! editor has ended.
use fuxix::Errno;
use fuxix::poll::{Events, PollFd, poll};
use fuxix::process::{Dispositions, Signal, kill_group};
use fuxix::pty::{self, Master};
use fuxix::terminal::{self, Size};
use std::time::{Duration, Instant};

fn size(rows: u16, cols: u16) -> Result<Size, String> {
    Size::new(rows, cols).ok_or_else(|| "a zero size".to_owned())
}

/// `cmd /c SCRIPT` on a new console, with `%BIN%` the editor.
fn start(script: &str) -> Result<(Master, fuxix::process::Job), String> {
    let _ = pty::load(std::path::Path::new("no-pinned-conpty"));
    let (master, slave) = pty::open(size(24, 40)?).map_err(|e| e.to_string())?;
    let job = slave
        .spawn(
            &["cmd.exe", "/c", script],
            |command| {
                command.env("BIN", env!("CARGO_BIN_EXE_fuxix-lineedit"));
            },
            Dispositions::Default,
        )
        .map_err(|e| e.to_string())?;
    Ok((master, job))
}

/// Reads the master into `seen` until it holds `needle`, for up to 10 s.
fn wait_for(master: &Master, seen: &mut String, needle: &str) -> Result<(), String> {
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut buffer = [0u8; 4096];
    while !seen.contains(needle) {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(format!("no {needle:?} in {seen:?}"));
        }
        let mut fds = [PollFd::new(master, Events::IN)];
        match poll(&mut fds, Some(left)) {
            Ok(_) | Err(Errno::INTR) => {}
            Err(e) => return Err(e.to_string()),
        }
        match fuxix::io::read(master, &mut buffer) {
            Ok(n) => seen.push_str(&String::from_utf8_lossy(
                buffer.get(..n).unwrap_or_default(),
            )),
            Err(Errno::AGAIN | Errno::INTR) => {}
            Err(e) => return Err(format!("{e}, no {needle:?} in {seen:?}")),
        }
    }
    Ok(())
}

fn send(master: &Master, bytes: &[u8]) -> Result<(), String> {
    let n = fuxix::io::write(master, bytes).map_err(|e| e.to_string())?;
    if n == bytes.len() {
        Ok(())
    } else {
        Err("short write".into())
    }
}

#[test]
fn lines_history_and_ctrl_d_end_with_the_shell_going_on() -> Result<(), String> {
    let (master, _job) = start("%BIN% & echo status=%errorlevel% & echo END")?;
    let mut seen = String::new();
    wait_for(&master, &mut seen, ">")?;
    send(&master, b"helo\x1b[Dl\x1b[F world\r")?;
    wait_for(&master, &mut seen, "hello world")?;
    seen.clear();
    // Up brings it back; Enter enters it again.
    send(&master, b"\x1b[A\r")?;
    wait_for(&master, &mut seen, "hello world")?;
    send(&master, b"\x04")?;
    wait_for(&master, &mut seen, "status=")?;
    wait_for(&master, &mut seen, "END")?;
    Ok(())
}

#[test]
fn a_resize_redraws() -> Result<(), String> {
    let (master, _job) = start("%BIN%")?;
    let mut seen = String::new();
    wait_for(&master, &mut seen, ">")?;
    send(&master, b"abcdefghijklmnopqrstuvwxyz")?;
    wait_for(&master, &mut seen, "z")?;
    seen.clear();
    terminal::set_window_size(&master, size(24, 10)?, None).map_err(|e| e.to_string())?;
    wait_for(&master, &mut seen, "uvwxyz")?;
    Ok(())
}
