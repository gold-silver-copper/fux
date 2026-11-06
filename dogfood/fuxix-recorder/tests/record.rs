//! The recorder run as a user runs it: on a terminal, and on none.
use fuxix::poll::{Events, PollFd};
use fuxix::terminal::Size;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;

fn scratch(name: &str) -> Result<std::path::PathBuf> {
    let dir = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join(name);
    std::fs::create_dir_all(&dir)?;
    Ok(dir.join("out.cast"))
}

#[test]
fn records_output_and_exit_status_without_a_terminal() -> Result {
    let cast = scratch("plain")?;
    let out = Command::new(env!("CARGO_BIN_EXE_fuxix-recorder"))
        .arg("-q")
        .arg("-o")
        .arg(&cast)
        .args(["--", "cmd.exe", "/c", "echo hello& exit 3"])
        .stdin(Stdio::null())
        .output()?;
    assert_eq!(out.status.code(), Some(3), "{out:?}");
    assert!(String::from_utf8(out.stdout)?.contains("hello"));
    let log = std::fs::read_to_string(&cast)?;
    let mut lines = log.lines();
    let header = lines.next().ok_or("no header")?;
    assert!(header.starts_with("{\"version\": 2, \"width\": "), "{header}");
    let events: String = lines.collect();
    assert!(events.contains("hello"), "{events}");
    Ok(())
}

/// On a terminal of its own: keys pass through, a resize reaches the
/// command and the log, and the output is passed and logged.
#[test]
fn follows_resizes_and_passes_keys_on_a_terminal() -> Result {
    let cast = scratch("tty")?;
    let size = |rows, cols| Size::new(rows, cols).ok_or("no size");
    let _ = fuxix::pty::load(std::path::Path::new("no-pinned-conpty"));
    let (master, slave) = fuxix::pty::open(size(10, 40)?)?;
    let cast_arg = cast.to_string_lossy().into_owned();
    let script = "mode con & set /p x=? & mode con & echo got:!x!";
    let argv = [env!("CARGO_BIN_EXE_fuxix-recorder"), "-q", "-o", &cast_arg, "--", "cmd.exe", "/v:on", "/c", script];
    let job = slave.spawn(&argv, |_| {}, fuxix::process::Dispositions::Default)?;

    let mut seen = Vec::new();
    let mut buffer = [0u8; 4096];
    let start = Instant::now();
    let mut step = 0;
    while start.elapsed() < Duration::from_secs(10) {
        let text = String::from_utf8_lossy(&seen).into_owned();
        if step == 0 && text.contains("40") {
            fuxix::terminal::set_window_size(&master, size(12, 50)?, None)?;
            std::thread::sleep(Duration::from_millis(200));
            fuxix::io::write(&master, b"abc\r")?;
            step = 1;
        }
        if text.contains("got:abc") {
            break;
        }
        let mut fds = [PollFd::new(&master, Events::IN)];
        fuxix::poll::poll(&mut fds, Some(Duration::from_millis(100)))?;
        match fuxix::io::read(&master, &mut buffer) {
            Ok(n) => seen.extend_from_slice(buffer.get(..n).unwrap_or_default()),
            Err(fuxix::Errno::AGAIN | fuxix::Errno::INTR) => {}
            Err(_) => break,
        }
    }
    let text = String::from_utf8_lossy(&seen);
    assert!(text.contains("50") && text.contains("got:abc"), "{text}");
    while job.ended()?.is_none() && start.elapsed() < Duration::from_secs(15) {
        std::thread::sleep(Duration::from_millis(50));
    }
    let log = std::fs::read_to_string(&cast)?;
    assert!(log.contains("\"r\", \"50x12\"]"), "{log}");
    assert!(log.contains("got:abc"), "{log}");
    Ok(())
}
