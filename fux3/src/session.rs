//! The session directory: one private directory holding a directory per
//! pane. A pane's holder locks its directory for as long as it lives, and
//! that lock is the only proof a pane is alive: a directory whose lock can
//! be taken is a dead pane's leftovers. The session's own lock (`.lock`)
//! serializes making and clearing pane directories; reads never take it.
use crate::{Failure, io};
use std::fs::File;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// The most panes a session holds.
pub const MAX_PANES: usize = 128;
/// The most entries a listing reads before it refuses.
const MAX_ENTRIES: usize = 1024;
/// How long a change waits for the session's lock.
const LOCK_WAIT: Duration = Duration::from_secs(5);
/// The session's lock file, a dot-name no pane can have.
const LOCK: &str = ".lock";

/// Refuses a name outside `[a-z0-9][a-z0-9_-]{0,31}`: no dot-names, so
/// nothing of the session's own (`.lock`, `.view`) is ever a pane.
pub fn check_name(name: &str) -> Result<(), Failure> {
    let lead = |c: char| c.is_ascii_lowercase() || c.is_ascii_digit();
    let mut chars = name.chars();
    let good = chars.next().is_some_and(lead)
        && chars.all(|c| lead(c) || c == '_' || c == '-')
        && name.len() <= 32;
    match good {
        true => Ok(()),
        false => Err(Failure::new(format!(
            "bad pane name {name:?}: it must match [a-z0-9][a-z0-9_-]{{0,31}}"
        ))),
    }
}

/// Opens `dir` as a directory to lock: a link, or anything but a
/// directory, is refused unopened.
fn open_dir(dir: &Path) -> std::io::Result<File> {
    File::options()
        .read(true)
        .custom_flags(fuxix::file::NOFOLLOW | fuxix::file::DIRECTORY | fuxix::file::NONBLOCK)
        .open(dir)
}

/// The lock on a pane's directory: `None` if another process holds it.
pub fn try_lock_dir(dir: &Path) -> Result<Option<File>, Failure> {
    let file = open_dir(dir).map_err(|e| io(dir.display(), e))?;
    match file.try_lock() {
        Ok(()) => Ok(Some(file)),
        Err(std::fs::TryLockError::WouldBlock) => Ok(None),
        Err(std::fs::TryLockError::Error(e)) => Err(io(dir.display(), e)),
    }
}

/// Whether `dir` is still the directory `held` names.
pub fn same_dir(dir: &Path, held: &File) -> bool {
    let held = held.metadata();
    let there = std::fs::symlink_metadata(dir);
    matches!((held, there), (Ok(a), Ok(b)) if a.dev() == b.dev() && a.ino() == b.ino())
}

/// Checks that `path` is a directory of this user's alone.
pub fn check_private(path: &Path) -> Result<(), Failure> {
    let shown = path.display();
    let meta = std::fs::symlink_metadata(path).map_err(|e| io(&shown, e))?;
    if !meta.is_dir() {
        return Err(Failure::new(format!(
            "{shown} is not a directory (a link is refused)"
        )));
    }
    if meta.uid() != fuxix::process::geteuid() {
        return Err(Failure::new(format!(
            "{shown} belongs to uid {}",
            meta.uid()
        )));
    }
    if meta.mode() & 0o077 != 0 {
        return Err(Failure::new(format!(
            "{shown} has mode {:o}, and must be 700",
            meta.mode() & 0o777
        )));
    }
    Ok(())
}

/// Makes `path` private if it is missing, then checks that it is.
fn private(path: &Path) -> Result<(), Failure> {
    match fuxix::file::private_dir(path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(io(path.display(), e)),
    }
    check_private(path)
}

/// The session directory when `FUX3_DIR` does not name one: in memory on
/// Linux (`$XDG_RUNTIME_DIR` is a tmpfs), on disk on macOS.
fn default_root() -> Result<PathBuf, Failure> {
    let set = |name: &str| std::env::var_os(name).filter(|v| !v.is_empty());
    let missing = |name: &str| Failure::new(format!("{name} is not set, and FUX3_DIR neither"));
    #[cfg(target_os = "macos")]
    let base = PathBuf::from(set("HOME").ok_or_else(|| missing("HOME"))?).join("Library/Caches");
    #[cfg(not(target_os = "macos"))]
    let base = PathBuf::from(set("XDG_RUNTIME_DIR").ok_or_else(|| missing("XDG_RUNTIME_DIR"))?);
    if !base.is_dir() {
        return Err(Failure::new(format!("{} is no directory", base.display())));
    }
    Ok(base.join("fux3"))
}

/// A session: its private directory.
#[derive(Clone, Debug)]
pub struct Session {
    pub root: PathBuf,
}

/// The session's lock, while held.
pub struct SessionLock {
    _file: File,
}

impl Session {
    /// The session `FUX3_DIR` names, else this user's default, made if
    /// missing and refused unless it is private.
    pub fn open() -> Result<Session, Failure> {
        let root = match std::env::var_os("FUX3_DIR").filter(|v| !v.is_empty()) {
            Some(root) => PathBuf::from(root),
            None => default_root()?,
        };
        if !root.is_absolute() {
            let why = format!("FUX3_DIR must be absolute, not {}", root.display());
            return Err(Failure::new(why));
        }
        private(&root)?;
        Ok(Session { root })
    }

    /// The directory of the pane called `name`.
    pub fn pane_dir(&self, name: &str) -> PathBuf {
        self.root.join(name)
    }

    /// Takes the session's lock, waiting a bounded time.
    pub fn lock(&self) -> Result<SessionLock, Failure> {
        let path = self.root.join(LOCK);
        let mut options = File::options();
        options.read(true).write(true).create(true).truncate(false);
        let file = fuxix::file::private_file(&mut options)
            .open(&path)
            .map_err(|e| io(path.display(), e))?;
        let start = Instant::now();
        loop {
            match file.try_lock() {
                Ok(()) => return Ok(SessionLock { _file: file }),
                Err(std::fs::TryLockError::WouldBlock) => {}
                Err(std::fs::TryLockError::Error(e)) => return Err(io(path.display(), e)),
            }
            if start.elapsed() >= LOCK_WAIT {
                let why = format!("{} was held for {LOCK_WAIT:?}", path.display());
                return Err(Failure::new(why));
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// The names with a pane directory here, live or not, sorted. Links
    /// and anything that is not a directory are no pane. Nothing is opened.
    pub fn names(&self) -> Result<Vec<String>, Failure> {
        let entries = std::fs::read_dir(&self.root).map_err(|e| io(self.root.display(), e))?;
        let mut names = Vec::new();
        for (seen, entry) in entries.flatten().enumerate() {
            if seen >= MAX_ENTRIES {
                let why = format!("more than {MAX_ENTRIES} entries in {}", self.root.display());
                return Err(Failure::new(why));
            }
            let Ok(name) = entry.file_name().into_string() else {
                continue;
            };
            let real = entry.file_type().is_ok_and(|kind| kind.is_dir());
            if real && check_name(&name).is_ok() {
                names.push(name);
            }
        }
        names.sort();
        Ok(names)
    }

    /// Whether `name`'s holder lives: its directory's lock is held. One
    /// that can't be looked at counts as live: it is never cleared on a
    /// guess.
    pub fn is_live(&self, name: &str) -> bool {
        !matches!(try_lock_dir(&self.pane_dir(name)), Ok(Some(_)))
    }

    /// The live panes, after clearing every dead one's leftovers.
    pub fn live(&self) -> Result<Vec<String>, Failure> {
        let lock = self.lock()?;
        self.clear_dead(&lock)
    }

    /// Removes the directory of every pane whose lock nobody holds, and
    /// gives the names of those that remain.
    pub fn clear_dead(&self, _lock: &SessionLock) -> Result<Vec<String>, Failure> {
        let mut live = Vec::new();
        for name in self.names()? {
            let dir = self.pane_dir(&name);
            match try_lock_dir(&dir) {
                Ok(Some(held)) => {
                    // Leftovers that will not go stay for whoever asks
                    // for that name, which says why.
                    let _ = remove_pane(&dir, &held);
                }
                Ok(None) | Err(_) => live.push(name),
            }
        }
        Ok(live)
    }

    /// Takes `name`, under the session's lock: its directory made and its
    /// lock held. A live pane's name, a full session, or leftovers that
    /// cannot be removed are refused, saying why.
    pub fn claim(&self, name: &str) -> Result<(PathBuf, File), Failure> {
        check_name(name)?;
        let lock = self.lock()?;
        let live = self.clear_dead(&lock)?;
        if live.iter().any(|n| n == name) {
            return Err(Failure::new(format!("pane {name} exists")));
        }
        let dir = self.pane_dir(name);
        if std::fs::symlink_metadata(&dir).is_ok() {
            return Err(Failure::new(format!(
                "{} is there and could not be cleared",
                dir.display()
            )));
        }
        if live.len() >= MAX_PANES {
            return Err(Failure::new(format!(
                "{} panes, at most {MAX_PANES}",
                live.len()
            )));
        }
        fuxix::file::private_dir(&dir).map_err(|e| io(dir.display(), e))?;
        match try_lock_dir(&dir)? {
            Some(held) => Ok((dir, held)),
            None => Err(Failure::new(format!("pane {name} exists"))),
        }
    }
}

/// Removes a pane's directory and what is in it, unless the directory
/// there is no longer the one `held` names: one moved aside and replaced
/// is another pane's. Links in it are removed, never followed.
pub fn remove_pane(dir: &Path, held: &File) -> Result<(), Failure> {
    if !same_dir(dir, held) {
        return Ok(());
    }
    std::fs::remove_dir_all(dir).map_err(|e| io(format!("removing {}", dir.display()), e))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_never_dot_names_or_paths() {
        for good in ["build", "a", "0-x_y"] {
            assert!(check_name(good).is_ok(), "{good}");
        }
        for bad in [
            "",
            ".view",
            ".lock",
            "a/b",
            "..",
            "Build",
            &std::iter::repeat_n('x', 33).collect::<String>(),
        ] {
            assert!(check_name(bad).is_err(), "{bad}");
        }
    }
}
