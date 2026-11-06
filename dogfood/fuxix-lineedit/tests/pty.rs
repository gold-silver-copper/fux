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

/// `sh -c SCRIPT` on a new PTY, with `$BIN` the editor.
fn start(script: &str) -> Result<(Master, std::process::Child), String> {
    let (master, slave) = pty::open(size(24, 40)?).map_err(|e| e.to_string())?;
    let (child, _) = slave
        .spawn(&["sh", "-c", script], Dispositions::Default, |command| {
            command.env("BIN", env!("CARGO_BIN_EXE_fuxix-lineedit"));
        })
        .map_err(|e| e.to_string())?;
    Ok((master, child))
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

/// Whether the terminal has the modes a shell needs back.
fn cooked(master: &Master) -> Result<bool, String> {
    let modes = terminal::attributes(master).map_err(|e| e.to_string())?;
    Ok(modes.echoes() && modes.edits_lines())
}

#[test]
fn lines_history_ctrl_z_and_ctrl_d_leave_the_terminal_cooked() -> Result<(), String> {
    let (master, mut child) = start("$BIN; echo status=$?; echo END")?;
    let mut seen = String::new();
    wait_for(&master, &mut seen, "> ")?;
    send(&master, b"helo\x1b[Dl\x1b[F world\r")?;
    wait_for(&master, &mut seen, "=> hello world")?;
    seen.clear();
    // Up brings it back; Enter enters it again.
    send(&master, b"\x1b[A\r")?;
    wait_for(&master, &mut seen, "=> hello world")?;
    // Ctrl-Z stops it with the terminal cooked: the terminal echoes.
    send(&master, b"\x1a")?;
    wait_for(&master, &mut seen, "\x1b[2C\r\n")?;
    send(&master, b"zz")?;
    // Echoed by the terminal straight after, not drawn by the editor.
    wait_for(&master, &mut seen, "\x1b[2C\r\nzz")?;
    let group = terminal::foreground_group(&master)
        .map_err(|e| e.to_string())?
        .ok_or("no foreground group")?;
    kill_group(group, Signal::Cont).map_err(|e| e.to_string())?;
    seen.clear();
    send(&master, b"\x15after\r")?;
    wait_for(&master, &mut seen, "=> after")?;
    send(&master, b"\x04")?;
    wait_for(&master, &mut seen, "status=0")?;
    wait_for(&master, &mut seen, "END")?;
    assert!(cooked(&master)?, "{seen}");
    child.wait().map_err(|e| e.to_string())?;
    Ok(())
}

#[test]
fn a_resize_redraws_and_sigterm_restores_the_terminal() -> Result<(), String> {
    // sh ignores TERM, so that only the editor, which catches it, ends.
    let (master, mut child) = start("trap '' TERM; $BIN; echo status=$?; echo END")?;
    let mut seen = String::new();
    wait_for(&master, &mut seen, "> ")?;
    send(&master, b"abcdefghijklmnopqrstuvwxyz")?;
    wait_for(&master, &mut seen, "z")?;
    seen.clear();
    // 10 columns: the prompt, the line's last 6 letters, the cursor, and
    // the last column left empty.
    terminal::set_window_size(&master, size(24, 10)?, None).map_err(|e| e.to_string())?;
    wait_for(&master, &mut seen, "> uvwxyz")?;
    let group = terminal::foreground_group(&master)
        .map_err(|e| e.to_string())?
        .ok_or("no foreground group")?;
    kill_group(group, Signal::Term).map_err(|e| e.to_string())?;
    wait_for(&master, &mut seen, "status=143")?;
    wait_for(&master, &mut seen, "END")?;
    assert!(cooked(&master)?, "{seen}");
    child.wait().map_err(|e| e.to_string())?;
    Ok(())
}
