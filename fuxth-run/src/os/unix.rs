//! What the runner does differently on Unix: key mode by `stty`, Ctrl-Z,
//! a FIFO opened without waiting, and a file's mode bits.
use crate::input::{Input, Stop, errno, utf8};
use crate::words::IO;
use fuxix::Errno;
use fuxix::process::{Pid, Signal, kill};
use fuxix::terminal::{Termios, attributes, set_attributes};
use fuxth::{Code, Failure};
use std::os::fd::{AsFd, BorrowedFd};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::Path;

/// What a read of stdin waits on.
pub type Waited<'a> = BorrowedFd<'a>;

/// stdin, to wait on.
pub fn stdin() -> Waited<'static> {
    static STDIN: std::sync::OnceLock<std::io::Stdin> = std::sync::OnceLock::new();
    STDIN.get_or_init(std::io::stdin).as_fd()
}

/// Reads stdin: how many bytes, 0 at its end. A key mode changes nothing
/// here: the terminal gives what its modes say.
pub fn read_stdin(_keys: bool, buffer: &mut [u8]) -> Result<Option<usize>, Errno> {
    fuxix::io::read(std::io::stdin(), buffer).map(Some)
}

/// The terminal's modes as found, and for keys once `stty` made them;
/// and whether its own modes read a CR typed as a newline (`ICRNL`).
pub struct Modes {
    cooked: Termios,
    raw: Option<Termios>,
    pub crnl: bool,
}

impl Modes {
    /// The terminal's modes, kept.
    pub fn keep() -> Result<Modes, Failure> {
        let cooked =
            attributes(std::io::stdin()).map_err(|e| errno("reading the terminal's modes", e))?;
        Ok(Modes {
            cooked,
            raw: None,
            crnl: crnl()?,
        })
    }

    /// The terminal in key mode: `stty`, the first time.
    pub fn keys(&mut self) -> Result<(), Failure> {
        let raw = match &self.raw {
            Some(raw) => raw.clone(),
            None => {
                stty()?;
                let raw = attributes(std::io::stdin())
                    .map_err(|e| errno("reading the terminal's modes", e))?;
                self.raw = Some(raw.clone());
                raw
            }
        };
        set_attributes(std::io::stdin(), &raw).map_err(|e| errno("setting the terminal's modes", e))
    }

    /// The terminal in its own modes again.
    pub fn own(&mut self) -> Result<(), Failure> {
        set_attributes(std::io::stdin(), &self.cooked)
            .map_err(|e| errno("restoring the terminal's modes", e))
    }
}

/// Whether the terminal on stdin reads a CR typed as a newline, as
/// `stty -a` says (`icrnl` or `-icrnl`).
fn crnl() -> Result<bool, Failure> {
    let ran = std::process::Command::new("stty")
        .arg("-a")
        .stdin(std::process::Stdio::inherit())
        .output()
        .map_err(|e| Failure::new(Code::Host(IO), format!("stty -a: {e}")))?;
    if !ran.status.success() {
        let said = String::from_utf8_lossy(&ran.stderr);
        return Err(Failure::new(
            Code::Host(IO),
            format!("stty -a: {}", said.trim()),
        ));
    }
    let said = String::from_utf8_lossy(&ran.stdout);
    Ok(said.split_whitespace().any(|flag| flag == "icrnl"))
}

/// Puts the terminal on stdin in key mode, as `stty` does it.
fn stty() -> Result<(), Failure> {
    let ran = std::process::Command::new("stty")
        .args([
            "-icanon", "-echo", "-iexten", "-ixon", "-icrnl", "min", "1", "time", "0",
        ])
        .stdin(std::process::Stdio::inherit())
        .output()
        .map_err(|e| Failure::new(Code::Host(IO), format!("stty: {e}")))?;
    match ran.status.success() {
        true => Ok(()),
        false => {
            let said = String::from_utf8_lossy(&ran.stderr);
            Err(Failure::new(
                Code::Host(IO),
                format!("stty: {}", said.trim()),
            ))
        }
    }
}

/// Stops this process, as Ctrl-Z asks.
pub fn stop_self() -> Result<(), Failure> {
    let me = i32::try_from(std::process::id())
        .ok()
        .and_then(Pid::from_raw);
    if let Some(me) = me {
        kill(me, Signal::Stop).map_err(|e| errno("stopping", e))?;
    }
    Ok(())
}

/// The text of what is at `path`, no plain file (a FIFO, `<(…)`), read as
/// stdin is, so that Ctrl-C reaches the program meanwhile.
pub fn read_waiting(input: &mut Input, path: &Path) -> Result<std::io::Result<String>, Stop> {
    let open = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(path);
    let file = match open {
        Ok(file) => file,
        Err(e) => return Ok(Err(e)),
    };
    let mut bytes = Vec::new();
    let mut buffer = [0; 4096];
    loop {
        if !input.wait(file.as_fd(), None)? {
            continue;
        }
        match fuxix::io::read(&file, &mut buffer) {
            Ok(0) => break,
            Ok(n) => bytes.extend(buffer.get(..n).unwrap_or_default()),
            Err(Errno::AGAIN | Errno::INTR) => {}
            Err(e) => return Ok(Err(std::io::Error::from_raw_os_error(e.raw()))),
        }
    }
    Ok(utf8(bytes))
}

/// A file's mode bits.
pub fn file_mode(meta: &std::fs::Metadata) -> i64 {
    i64::from(meta.mode() & 0o7777)
}

/// Sets a file's mode bits.
pub fn set_file_mode(path: &Path, mode: u32) -> std::io::Result<()> {
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
}
