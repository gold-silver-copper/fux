//! A program on a PTY of its own, as a terminal runs it: `feel`'s stand-in
//! for the terminal a person types in. A thread reads everything the
//! program writes as it comes, counting it, and notes when a pattern it is
//! told to watch for arrives in the text written: escape sequences are
//! skipped, as a multiplexer that moves the cursor before each cell
//! (herdr) writes a pattern's characters apart.
use std::os::fd::{AsFd, OwnedFd};
use std::process::{Child, Command};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::{Duration, Instant};

/// What the reader has seen.
#[derive(Default)]
struct Seen {
    /// Bytes read in all.
    total: u64,
    /// Paints begun: each `CSI ? 2026 h` (fux begins every paint with one).
    frames: u64,
    /// The last bytes read, to find a paint's start split between reads.
    tail: Vec<u8>,
    /// The last of the text written, escape sequences skipped, to find a
    /// pattern split between reads.
    text: Vec<u8>,
    /// Where the text filter is in an escape sequence.
    escape: Escape,
    /// A pattern watched for, and when it arrived.
    watch: Option<Vec<u8>>,
    arrived: Option<Instant>,
    /// The program closed the terminal.
    closed: bool,
}

const BEGIN: &[u8] = b"\x1b[?2026h";

/// Where `text` is in what it reads: in text, or in an escape sequence,
/// a control sequence (CSI), or a string (OSC, DCS, SOS, PM, APC).
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
enum Escape {
    #[default]
    Text,
    Esc,
    Csi,
    Str,
    StrEsc,
}

/// Appends the text in `bytes` to `out`: what is not an escape sequence
/// nor a C0 control, the state carried from one read to the next.
fn text(bytes: &[u8], state: &mut Escape, out: &mut Vec<u8>) {
    for &b in bytes {
        *state = match (*state, b) {
            (_, 0x18 | 0x1a) => Escape::Text,
            (Escape::Text, 0x1b) => Escape::Esc,
            (Escape::Text, b) if b < 0x20 || b == 0x7f => Escape::Text,
            (Escape::Text, b) => {
                out.push(b);
                Escape::Text
            }
            (Escape::Esc, b'[') => Escape::Csi,
            (Escape::Esc, b']' | b'P' | b'X' | b'^' | b'_') => Escape::Str,
            (Escape::Esc, 0x20..=0x2f) => Escape::Esc,
            (Escape::Esc, _) => Escape::Text,
            (Escape::Csi, 0x40..=0x7e) => Escape::Text,
            (Escape::Csi, _) => Escape::Csi,
            (Escape::Str | Escape::StrEsc, 0x07) => Escape::Text,
            (Escape::Str | Escape::StrEsc, 0x1b) => Escape::StrEsc,
            (Escape::StrEsc, b'\\') => Escape::Text,
            (Escape::Str | Escape::StrEsc, _) => Escape::Str,
        };
    }
}

/// Whether `pattern` is in `window` ending past `old` (its first `old`
/// bytes were looked at before).
fn found(window: &[u8], old: usize, pattern: &[u8]) -> usize {
    if pattern.is_empty() {
        return 0;
    }
    let mut count = 0usize;
    let mut at = 0usize;
    while let Some(rest) = window.get(at..) {
        let Some(i) = rest.windows_checked(pattern) else {
            break;
        };
        let start = at.saturating_add(i);
        if start.saturating_add(pattern.len()) > old {
            count = count.saturating_add(1);
        }
        at = start.saturating_add(1);
    }
    count
}

/// Where `pattern` first starts in a slice.
trait Find {
    fn windows_checked(&self, pattern: &[u8]) -> Option<usize>;
}

impl Find for [u8] {
    fn windows_checked(&self, pattern: &[u8]) -> Option<usize> {
        let last = self.len().checked_sub(pattern.len())?;
        (0..=last).find(|&i| self.get(i..).is_some_and(|rest| rest.starts_with(pattern)))
    }
}

pub struct Terminal {
    master: OwnedFd,
    pub child: Child,
    seen: Arc<(Mutex<Seen>, Condvar)>,
}

impl Terminal {
    /// Starts `program` with `args` on a PTY of `rows` by `cols`, with
    /// `env` added to this process's environment (less `feel::FOREIGN`),
    /// watching for `watch` from its first byte.
    pub fn start(
        rows: u16,
        cols: u16,
        program: &str,
        args: &[&str],
        env: &[(&str, &str)],
        watch: Option<&[u8]>,
    ) -> Result<Terminal, String> {
        let (master, slave) =
            fuxix::pty::open(rows, cols).map_err(|e| format!("opening a PTY: {e}"))?;
        let me = std::env::current_exe().map_err(|e| e.to_string())?;
        let clone = |fd: &OwnedFd| fd.try_clone().map_err(|e| format!("the PTY: {e}"));
        let mut command = Command::new(me);
        command
            .arg("__launch")
            .arg(program)
            .args(args)
            .env("TERM", "xterm-256color")
            .stdin(clone(&slave)?)
            .stdout(clone(&slave)?)
            .stderr(clone(&slave)?);
        for name in crate::feel::FOREIGN {
            command.env_remove(name);
        }
        for (key, value) in env {
            command.env(key, value);
        }
        let child = command
            .spawn()
            .map_err(|e| format!("starting {program}: {e}"))?;
        drop(slave);
        let seen = Seen {
            watch: watch.map(<[u8]>::to_vec),
            ..Seen::default()
        };
        let seen = Arc::new((Mutex::new(seen), Condvar::new()));
        let reader = master.try_clone().map_err(|e| e.to_string())?;
        let shared = Arc::clone(&seen);
        std::thread::spawn(move || read(&reader, &shared));
        Ok(Terminal {
            master,
            child,
            seen,
        })
    }

    /// Types `bytes`, as a person at the terminal.
    pub fn type_bytes(&self, bytes: &[u8]) -> Result<(), String> {
        let mut rest = bytes;
        while !rest.is_empty() {
            let n = fuxix::io::write(self.master.as_fd(), rest).map_err(|e| e.to_string())?;
            rest = rest.get(n..).unwrap_or_default();
        }
        Ok(())
    }

    /// Watches for `pattern` from now on, forgetting what came before.
    pub fn watch(&self, pattern: &[u8]) {
        let (lock, _) = &*self.seen;
        let mut seen = lock.lock().unwrap_or_else(PoisonError::into_inner);
        seen.tail.clear();
        seen.text.clear();
        seen.watch = Some(pattern.to_vec());
        seen.arrived = None;
    }

    /// Waits up to `limit` for the pattern watched for; when it arrived.
    pub fn arrival(&self, limit: Duration) -> Option<Instant> {
        let deadline = Instant::now().checked_add(limit)?;
        let (lock, wake) = &*self.seen;
        let mut seen = lock.lock().unwrap_or_else(PoisonError::into_inner);
        loop {
            if let Some(at) = seen.arrived {
                return Some(at);
            }
            let left = deadline.checked_duration_since(Instant::now())?;
            if seen.closed || left.is_zero() {
                return None;
            }
            seen = wake
                .wait_timeout(seen, left)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }

    /// Bytes and paints read so far.
    pub fn counts(&self) -> (u64, u64) {
        let (lock, _) = &*self.seen;
        let seen = lock.lock().unwrap_or_else(PoisonError::into_inner);
        (seen.total, seen.frames)
    }

    pub fn pid(&self) -> u32 {
        self.child.id()
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        if let Some(pid) = fuxix::process::Pid::of(&self.child) {
            let _ = fuxix::process::kill_group(pid, fuxix::process::Signal::Hup);
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// The reader: everything from `master`, until the program closes it.
fn read(master: &OwnedFd, seen: &(Mutex<Seen>, Condvar)) {
    let mut buffer = vec![0u8; 1 << 16];
    let (lock, wake) = seen;
    loop {
        let n = match fuxix::io::read(master.as_fd(), &mut buffer) {
            Ok(0) | Err(_) => 0,
            Ok(n) => n,
        };
        let now = Instant::now();
        let mut s = lock.lock().unwrap_or_else(PoisonError::into_inner);
        if n == 0 {
            s.closed = true;
            drop(s);
            wake.notify_all();
            return;
        }
        let chunk = buffer.get(..n).unwrap_or_default();
        s.total = s.total.saturating_add(u64::try_from(n).unwrap_or(0));
        let old = s.tail.len();
        let mut window = std::mem::take(&mut s.tail);
        window.extend_from_slice(chunk);
        let frames = found(&window, old, BEGIN);
        s.frames = s.frames.saturating_add(u64::try_from(frames).unwrap_or(0));
        let keep = window.len().saturating_sub(64);
        s.tail = window.get(keep..).unwrap_or_default().to_vec();
        if s.arrived.is_none() && s.watch.is_some() {
            let mut written = std::mem::take(&mut s.text);
            let old = written.len();
            let mut state = s.escape;
            text(chunk, &mut state, &mut written);
            s.escape = state;
            if s.watch
                .as_ref()
                .is_some_and(|p| found(&written, old, p) > 0)
            {
                s.arrived = Some(now);
            }
            let keep = written.len().saturating_sub(64);
            s.text = written.get(keep..).unwrap_or_default().to_vec();
        }
        drop(s);
        wake.notify_all();
    }
}

#[cfg(test)]
mod tests {
    /// A pattern is found in the text, whatever escape sequences come
    /// between its characters, and only in the text.
    #[test]
    fn text_skips_escape_sequences_across_reads() {
        let mut state = super::Escape::default();
        let mut out = Vec::new();
        let painted = "\u{2603}\x1b[1;2H\u{2603}\x1b]8;;\x1b\\\u{2603}\x1b[0m";
        let (a, b) = painted.as_bytes().split_at_checked(7).unwrap_or_default();
        super::text(a, &mut state, &mut out);
        super::text(b, &mut state, &mut out);
        assert_eq!(out, "\u{2603}\u{2603}\u{2603}".as_bytes());
        // The text of a sequence is not text.
        let mut out = Vec::new();
        super::text(b"\x1b]2;hidden\x07shown\x1b[31mred", &mut state, &mut out);
        assert_eq!(out, b"shownred");
    }

    #[test]
    fn a_pattern_is_found_once_across_reads() {
        let begin = super::BEGIN;
        let window = b"ab\x1b[?2026hcd\x1b[?2026h";
        assert_eq!(super::found(window, 0, begin), 2);
        // The first was in the bytes looked at before.
        assert_eq!(super::found(window, 12, begin), 1);
        // Split between two reads: found in the second.
        assert_eq!(super::found(b"\x1b[?20", 0, begin), 0);
        assert_eq!(super::found(b"\x1b[?2026h", 5, begin), 1);
    }
}
