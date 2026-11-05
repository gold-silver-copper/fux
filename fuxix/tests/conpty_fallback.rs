//! The built-in ConPTY is used only when `load` said why (WINDOWS.md,
//! PR 6), and never a package or file that isn't the pin. Its own binary,
//! as `load` chooses once per process.
#![cfg(windows)]
use fuxix::io::read;
use fuxix::poll::{Events, Poll, PollFd};
use fuxix::process::{Dispositions, Reach, Status};
use fuxix::pty;
use fuxix::terminal::Size;
use std::io::Write;
use std::path::Path;
use std::time::{Duration, Instant};

type Outcome = Result<(), String>;

const PACKAGE: &str = "microsoft.windows.console.conpty.1.25.260930003.nupkg";
#[cfg(target_arch = "x86_64")]
const PIN: &str = "1.25.260930003-x64";
#[cfg(target_arch = "aarch64")]
const PIN: &str = "1.25.260930003-arm64";

fn text(error: impl std::fmt::Display) -> String {
    error.to_string()
}

#[test]
fn a_tampered_conpty_is_never_loaded_and_the_built_in_is_said() -> Outcome {
    let deadline = Instant::now()
        .checked_add(Duration::from_secs(20))
        .ok_or("a deadline")?;
    let root = Path::new(env!("CARGO_TARGET_TMPDIR")).join("conpty-fallback");
    let _ = std::fs::remove_dir_all(&root);
    let size = Size::new(24, 80).ok_or("a size")?;

    // Before `load`, no console is made with either ConPTY.
    let before = pty::open(size).err().ok_or("open before load worked")?;
    if !before.to_string().starts_with("load") {
        return Err(format!("open before load: {before}"));
    }

    // A package one byte off fails its hash, and nothing is installed.
    let flipped = root.join("flipped");
    std::fs::create_dir_all(&flipped).map_err(text)?;
    let mut bytes = match std::env::var_os("FUXIX_CONPTY_PACKAGE") {
        Some(package) => std::fs::read(package).map_err(text)?,
        None => b"PK\x03\x04 not the package".to_vec(),
    };
    let middle = 1000.min(bytes.len().saturating_sub(1));
    if let Some(byte) = bytes.get_mut(middle) {
        *byte ^= 1;
    }
    std::fs::write(flipped.join(PACKAGE), &bytes).map_err(text)?;
    let fetched = pty::fetch(&flipped)
        .err()
        .ok_or("a flipped package fetched")?;
    if !fetched.to_string().contains("not the pinned") || flipped.join(PIN).exists() {
        return Err(format!("a flipped package: {fetched}"));
    }

    // A planted folder whose files aren't the pin is not loaded: the
    // built-in is, and `load` says why.
    let planted = root.join("planted");
    std::fs::create_dir_all(planted.join(PIN)).map_err(text)?;
    for name in ["conpty.dll", "OpenConsole.exe"] {
        std::fs::write(planted.join(PIN).join(name), b"MZ planted").map_err(text)?;
    }
    let loaded = pty::load(&planted)
        .err()
        .ok_or("a planted conpty.dll loaded")?;
    if !loaded.to_string().contains("not the pinned") {
        return Err(format!("load of a planted folder: {loaded}"));
    }
    let _ = writeln!(std::io::stderr(), "FALLBACK fetch: {fetched}");
    let _ = writeln!(std::io::stderr(), "FALLBACK load: {loaded}");

    // The built-in runs programs.
    let (master, slave) = pty::open(size).map_err(text)?;
    let system = std::env::var_os("SystemRoot").ok_or("no SystemRoot")?;
    let cmd = Path::new(&system).join("System32").join("cmd.exe");
    let job = slave
        .spawn(
            &[
                cmd.as_os_str(),
                "/d".as_ref(),
                "/c".as_ref(),
                "echo fux-fallback".as_ref(),
            ],
            |_| {},
            Dispositions::Default,
        )
        .map_err(text)?;
    let mut poll = Poll::default();
    let mut output = Vec::new();
    let mut buffer = vec![0u8; 65536];
    let ended = 'read: loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            let _ = job.kill(Reach::Session);
            return Err(format!(
                "deadline, after «{}»",
                String::from_utf8_lossy(&output)
            ));
        }
        let mut list = poll.list();
        list.push(PollFd::new(&master, Events::IN));
        list.poll(Some(left.min(Duration::from_millis(100))))
            .map_err(text)?;
        drop(list);
        loop {
            match read(&master, &mut buffer) {
                Ok(0) => break 'read job.ended().map_err(text)?,
                Ok(n) => output.extend_from_slice(buffer.get(..n).unwrap_or_default()),
                Err(fuxix::Errno::AGAIN) => break,
                Err(errno) => return Err(format!("read: {errno}")),
            }
        }
    };
    let seen = (0..output.len()).any(|at| {
        output
            .get(at..)
            .is_some_and(|rest| rest.starts_with(b"fux-fallback"))
    });
    if !seen || ended != Some(Status::Exited(0)) {
        return Err(format!(
            "through the built-in: {ended:?}, «{}»",
            String::from_utf8_lossy(&output)
        ));
    }
    let _ = writeln!(
        std::io::stderr(),
        "FALLBACK built-in ran cmd: {ended:?}, {} bytes",
        output.len()
    );
    Ok(())
}
