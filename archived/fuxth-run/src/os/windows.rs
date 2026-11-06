//! What the runner does differently on Windows: key mode is the console's
//! own (`fuxix::terminal::Raw`, Ctrl-C and Ctrl-Break still signals), Ctrl-Z
//! is a key, and a file's mode is read-only or not.
use crate::input::{Input, Stop, errno, utf8};
use fuxix::Errno;
use fuxix::terminal::Raw;
use fuxth::Failure;
use std::io::{IsTerminal as _, Read as _, Stdin};
use std::path::Path;
use std::sync::OnceLock;

/// What a read of stdin waits on.
pub type Waited<'a> = &'a Stdin;

/// stdin, to wait on.
pub fn stdin() -> Waited<'static> {
    static STDIN: OnceLock<Stdin> = OnceLock::new();
    STDIN.get_or_init(std::io::stdin)
}

/// Reads stdin: how many bytes, 0 at its end; none if what it held was no
/// bytes (a key's release). On a console, a key's bytes in key mode. In its
/// own modes it edits and echoes a line, which std reads as UTF-8: the
/// console, not the runner, then holds the wait, and Ctrl-C at a half-typed
/// line acts at Enter, as std goes on with a read it ended.
pub fn read_stdin(keys: bool, buffer: &mut [u8]) -> Result<Option<usize>, Errno> {
    if !keys && stdin().is_terminal() {
        let mut console = stdin();
        let count = console.read(buffer).map_err(|_| Errno::IO)?;
        // A console's line ends CR LF; what the read didn't take, std keeps.
        let read = buffer.get(..count).unwrap_or_default();
        let ends = |at: &usize| {
            read.get(*at).copied() == Some(b'\r')
                && read.get(at.saturating_add(1)).copied() == Some(b'\n')
        };
        let kept: Vec<u8> = (0..count)
            .filter(|at| !ends(at))
            .filter_map(|at| read.get(at).copied())
            .collect();
        for (to, from) in buffer.iter_mut().zip(&kept) {
            *to = *from;
        }
        return Ok(Some(kept.len()));
    }
    match fuxix::io::read(stdin(), buffer) {
        Ok(count) => Ok(Some(count)),
        Err(Errno::AGAIN) => Ok(None),
        Err(errno) => Err(errno),
    }
}

/// The console's modes as found, held while it is in key mode. A CR typed
/// in key mode is what the console's own modes would have made a newline.
#[derive(Default)]
pub struct Modes {
    raw: Option<Raw<Stdin>>,
    pub crnl: bool,
}

impl Modes {
    /// Nothing to keep till key mode: `Raw` saves what it changes.
    pub fn keep() -> Result<Modes, Failure> {
        Ok(Modes {
            raw: None,
            crnl: true,
        })
    }

    /// The console in key mode, its signals kept.
    pub fn keys(&mut self) -> Result<(), Failure> {
        let raw = Raw::enter_keeping_signals(std::io::stdin())
            .map_err(|e| errno("setting the console's modes", e))?;
        self.raw = Some(raw);
        Ok(())
    }

    /// The console in its own modes again.
    pub fn own(&mut self) -> Result<(), Failure> {
        self.raw = None;
        Ok(())
    }
}

/// Windows has no Ctrl-Z to answer: it is a key.
pub fn stop_self() -> Result<(), Failure> {
    Ok(())
}

/// The text of what is at `path`, no plain file. Nothing at a path here
/// waits as a FIFO does.
pub fn read_waiting(_: &mut Input, path: &Path) -> Result<std::io::Result<String>, Stop> {
    Ok(std::fs::read(path).and_then(utf8))
}

/// What Windows keeps of a file's mode bits: read-only, or not.
pub fn file_mode(meta: &std::fs::Metadata) -> i64 {
    match meta.permissions().readonly() {
        true => 0o444,
        false => 0o666,
    }
}

/// Sets what Windows keeps of a file's mode bits: read-only, with no
/// owner's write bit.
pub fn set_file_mode(path: &Path, mode: u32) -> std::io::Result<()> {
    let mut permissions = std::fs::metadata(path)?.permissions();
    permissions.set_readonly(mode & 0o200 == 0);
    std::fs::set_permissions(path, permissions)
}
