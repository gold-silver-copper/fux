//! The session directory: one private directory holding a `p-NAME` directory
//! per pane. A pane's daemon holds an OS lock on its claim for as long as it
//! lives, so a claim that can be taken belongs to nobody. The session's own
//! lock serializes changes to the directory and is never held for reads.
use crate::sys;
use crate::wire::{Code, Failure, MAX_LABEL_KEY, MAX_LABEL_VALUE, MAX_LABELS};
use std::fs::File;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// The most panes a session holds.
pub const MAX_PANES: usize = 128;
/// How long a change waits for the session's lock.
const LOCK_WAIT: Duration = Duration::from_secs(5);
const PREFIX: &str = "p-";
/// The most entries a listing reads before it refuses.
const MAX_ENTRIES: usize = 4 * MAX_PANES;

fn io(what: impl std::fmt::Display, error: impl std::fmt::Display) -> Failure {
    Failure::new(Code::Io, format!("{what}: {error}"))
}

/// Refuses a name outside `[a-z0-9][a-z0-9_-]{0,31}`.
pub fn check_name(name: &str) -> Result<(), Failure> {
    let lead = |c: char| c.is_ascii_lowercase() || c.is_ascii_digit();
    let mut chars = name.chars();
    let good = chars.next().is_some_and(lead)
        && chars.all(|c| lead(c) || c == '_' || c == '-')
        && name.len() <= 32;
    match good {
        true => Ok(()),
        false => Err(Failure::new(
            Code::Invalid,
            format!("bad pane name {name:?}: it must match [a-z0-9][a-z0-9_-]{{0,31}}"),
        )),
    }
}

/// Reads `key=value`, checked against the label limits. `name` is the
/// pane's own and cannot be set.
pub fn parse_label(text: &str) -> Result<(String, String), Failure> {
    let bad = |why: String| Failure::new(Code::Invalid, format!("bad label {text:?}: {why}"));
    let (key, value) = text
        .split_once('=')
        .ok_or_else(|| bad("it must be key=value".to_owned()))?;
    if key.is_empty() || key.len() > MAX_LABEL_KEY {
        return Err(bad(format!("a key is 1 to {MAX_LABEL_KEY} bytes")));
    }
    if value.len() > MAX_LABEL_VALUE {
        return Err(bad(format!("a value is at most {MAX_LABEL_VALUE} bytes")));
    }
    Ok((key.to_owned(), value.to_owned()))
}

/// Checks a whole set of labels given to `start`.
pub fn check_labels(labels: &[(String, String)]) -> Result<(), Failure> {
    if labels.len() > MAX_LABELS {
        let why = format!("{} labels, at most {MAX_LABELS}", labels.len());
        return Err(Failure::new(Code::Invalid, why));
    }
    for (index, (key, _)) in labels.iter().enumerate() {
        if key == "name" {
            let why = "the label `name` is the pane's own name";
            return Err(Failure::new(Code::Invalid, why));
        }
        if labels.iter().take(index).any(|(other, _)| other == key) {
            return Err(Failure::new(Code::Invalid, format!("label {key:?} twice")));
        }
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
    sys::check_private(path)
}

/// The directory a session lives in when `FUX2_DIR` does not say.
fn default_root() -> Result<(PathBuf, PathBuf), Failure> {
    let set = |name: &str| std::env::var_os(name).filter(|v| !v.is_empty());
    let missing = |name: &str| Failure::new(Code::Io, format!("{name} is not set"));
    #[cfg(target_os = "macos")]
    let base = PathBuf::from(set("HOME").ok_or_else(|| missing("HOME"))?).join("Library/Caches");
    #[cfg(all(unix, not(target_os = "macos")))]
    let base = match set("XDG_RUNTIME_DIR") {
        Some(run) => PathBuf::from(run),
        None => PathBuf::from(set("HOME").ok_or_else(|| missing("HOME"))?).join(".cache"),
    };
    #[cfg(windows)]
    let base = PathBuf::from(set("LOCALAPPDATA").ok_or_else(|| missing("LOCALAPPDATA"))?);
    let own = base.join("fux2");
    #[cfg(windows)]
    let session = {
        let user = fuxix::socket::User::current().map_err(|e| io("this user", e))?;
        let name = if user.elevated() {
            "default-elevated"
        } else {
            "default"
        };
        own.join("run").join(name)
    };
    #[cfg(unix)]
    let session = own.join("default");
    std::fs::create_dir_all(&base).map_err(|e| io(base.display(), e))?;
    Ok((own, session))
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
    /// The session `FUX2_DIR` names, else this user's default, made if
    /// missing and refused unless it is private.
    pub fn open() -> Result<Session, Failure> {
        if let Some(root) = std::env::var_os("FUX2_DIR").filter(|v| !v.is_empty()) {
            let root = PathBuf::from(root);
            if !root.is_absolute() {
                let why = format!("FUX2_DIR must be absolute, not {}", root.display());
                return Err(Failure::new(Code::Invalid, why));
            }
            private(&root)?;
            return Ok(Session { root });
        }
        let (own, root) = default_root()?;
        private(&own)?;
        #[cfg(windows)]
        private(&own.join("run"))?;
        private(&root)?;
        Ok(Session { root })
    }

    /// The directory of the pane called `name`.
    pub fn pane_dir(&self, name: &str) -> PathBuf {
        self.root.join(format!("{PREFIX}{name}"))
    }

    /// Takes the session's lock, waiting a bounded time.
    pub fn lock(&self) -> Result<SessionLock, Failure> {
        let now = Instant::now();
        self.lock_by(now.checked_add(LOCK_WAIT).unwrap_or(now))
    }

    /// Takes the session's lock, trying until `until`.
    pub fn lock_by(&self, until: Instant) -> Result<SessionLock, Failure> {
        let path = self.root.join("lock");
        let mut options = File::options();
        options.read(true).write(true).create(true).truncate(false);
        let file = fuxix::file::private_file(&mut options)
            .open(&path)
            .map_err(|e| io(path.display(), e))?;
        loop {
            match file.try_lock() {
                Ok(()) => return Ok(SessionLock { _file: file }),
                Err(std::fs::TryLockError::WouldBlock) => {}
                Err(std::fs::TryLockError::Error(e)) => return Err(io(path.display(), e)),
            }
            if Instant::now() >= until {
                let why = format!("{} is held by another fux2", path.display());
                return Err(Failure::new(Code::Busy, why));
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// The names with a pane directory here, sorted. Links and anything that
    /// is not a directory are no pane. Nothing is opened.
    pub fn names(&self) -> Result<Vec<String>, Failure> {
        let entries = std::fs::read_dir(&self.root).map_err(|e| io(self.root.display(), e))?;
        let mut names = Vec::new();
        for entry in entries.flatten() {
            let Ok(name) = entry.file_name().into_string() else {
                continue;
            };
            let Some(name) = name.strip_prefix(PREFIX) else {
                continue;
            };
            let real = entry.file_type().is_ok_and(|kind| kind.is_dir());
            if !real || check_name(name).is_err() {
                continue;
            }
            if names.len() >= MAX_ENTRIES {
                let why = format!(
                    "more than {MAX_ENTRIES} pane entries in {}",
                    self.root.display()
                );
                return Err(Failure::new(Code::Limit, why));
            }
            names.push(name.to_owned());
        }
        names.sort();
        Ok(names)
    }

    /// Removes the leftovers of every pane whose claim nobody holds, and
    /// says how many panes remain. Leftovers that will not go are left for
    /// whoever asks for that name.
    pub fn reclaim(&self, lock: &SessionLock) -> Result<usize, Failure> {
        let mut live = 0usize;
        for name in self.names()? {
            let dir = self.pane_dir(&name);
            match sys::try_claim(&dir) {
                Ok(Some(claim)) => drop(remove_pane(lock, &dir, claim)),
                Ok(None) => live = live.saturating_add(1),
                Err(_) => {}
            }
        }
        Ok(live)
    }

    /// Takes `name`, under the session's lock: its directory made and its
    /// claim held. A name another process holds is `exists`; a full session
    /// is `limit`; leftovers that cannot be removed say why.
    pub fn claim(&self, lock: &SessionLock, name: &str) -> Result<(PathBuf, File), Failure> {
        let live = self.reclaim(lock)?;
        let dir = self.pane_dir(name);
        if std::fs::symlink_metadata(&dir).is_ok() {
            return match sys::try_claim(&dir)? {
                // Nobody holds it, yet reclaiming left it: say why.
                Some(claim) => match remove_pane(lock, &dir, claim) {
                    Ok(()) => self.claim(lock, name),
                    Err(failure) => Err(failure),
                },
                None => Err(Failure::new(Code::Exists, format!("pane {name} exists"))),
            };
        }
        if live >= MAX_PANES {
            let why = format!("{live} panes, at most {MAX_PANES}");
            return Err(Failure::new(Code::Limit, why));
        }
        fuxix::file::private_dir(&dir).map_err(|e| io(dir.display(), e))?;
        match sys::try_claim(&dir)? {
            Some(claim) => Ok((dir, claim)),
            None => Err(Failure::new(Code::Exists, format!("pane {name} exists"))),
        }
    }

    /// Whether the pane `name` has a live daemon that cannot be reached
    /// yet or any more: its claim is held. Asked under the session's lock,
    /// tried until `until`; past it, the answer is `busy`, never a guess.
    pub fn claim_held(&self, name: &str, until: Instant) -> Result<bool, Failure> {
        let _lock = self.lock_by(until)?;
        let dir = self.pane_dir(name);
        let real = std::fs::symlink_metadata(&dir).is_ok_and(|meta| meta.is_dir());
        if !real {
            return Ok(false);
        }
        Ok(sys::try_claim(&dir)?.is_none())
    }
}

/// Removes a pane's directory, its claim given up first, as Windows needs
/// and every system allows. Nothing is removed unless the directory there
/// is still the one claimed: one moved aside and replaced is another pane's.
pub fn remove_pane(_lock: &SessionLock, dir: &Path, claim: File) -> Result<(), Failure> {
    if !sys::still_claimed(dir, &claim) {
        return Ok(());
    }
    sys::remove_endpoint(dir);
    drop(claim);
    let _ = std::fs::remove_file(dir.join("claim"));
    std::fs::remove_dir(dir).map_err(|e| io(format!("removing {}", dir.display()), e))
}

/// Whether `labels` (a pane's, its name among them) pass every `key=value`
/// test.
pub fn matches(name: &str, labels: &[(String, String)], tests: &[(String, String)]) -> bool {
    tests.iter().all(|(key, value)| {
        (key == "name" && value == name) || labels.iter().any(|(k, v)| k == key && v == value)
    })
}
