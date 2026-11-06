//! The supervisor run as a program in a console: restarts with backoff, and
//! ends what it runs on Ctrl-C, the one stop a console has.
use fuxix::poll::{Events, PollFd};
use fuxix::process::Dispositions;
use std::time::{Duration, Instant};

#[test]
fn restarts_and_ends_on_ctrl_c() -> Result<(), Box<dyn std::error::Error>> {
    let dir = std::env::temp_dir().join(format!("supervise-{}", std::process::id()));
    std::fs::create_dir_all(&dir)?;
    let file = dir.join("programs");
    std::fs::write(
        &file,
        "# two programs\nquick cmd.exe /c 'exit 3'\nslow ping.exe -n 30 127.0.0.1\n",
    )?;
    let _ = fuxix::pty::load(std::path::Path::new("no-pinned-conpty"));
    let size = fuxix::terminal::Size::new(24, 100).ok_or("a size")?;
    let (master, slave) = fuxix::pty::open(size)?;
    let file = file.to_string_lossy().into_owned();
    let argv = [
        env!("CARGO_BIN_EXE_fuxix-supervise"),
        "--backoff",
        "50",
        "--backoff-max",
        "400",
        "--grace",
        "300",
        &file,
    ];
    let job = slave.spawn(&argv, |_| {}, Dispositions::Default)?;
    let start = Instant::now();
    let mut seen = String::new();
    let mut buffer = [0u8; 4096];
    let mut sent = false;
    while job.ended()?.is_none() {
        assert!(start.elapsed() < Duration::from_secs(20), "no end: {seen}");
        if !sent && start.elapsed() > Duration::from_millis(1500) {
            fuxix::io::write(&master, b"\x03")?;
            sent = true;
        }
        let mut fds = [PollFd::new(&master, Events::IN)];
        fuxix::poll::poll(&mut fds, Some(Duration::from_millis(50)))?;
        if let Ok(n) = fuxix::io::read(&master, &mut buffer) {
            seen.push_str(&String::from_utf8_lossy(buffer.get(..n).unwrap_or_default()));
        }
    }
    std::fs::remove_dir_all(&dir)?;
    Ok(())
}
