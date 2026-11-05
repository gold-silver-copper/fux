//! The files a program reads (`include`, `require`, `--file` and the
//! config): any readable file, a FIFO or `<(…)` among them, relative to the
//! file naming it, else to where fux started. A config's are kept as they
//! were read, so its definitions are read again from the same text.
use super::{Config, Failed};

use std::io;
use std::path::{Path, PathBuf};

/// The text of the file at `path`, read to its end: UTF-8, of any kind of
/// file that reads.
pub(super) fn read(path: &Path) -> io::Result<String> {
    let bytes = std::fs::read(path)?;
    String::from_utf8(bytes)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "not UTF-8 text"))
}

/// The one identity of the file at `path`: its canonical path; or, for a
/// file that has none (`<(…)`'s `/dev/fd/63`, a pipe), the path itself.
pub(super) fn identity(path: &Path) -> io::Result<PathBuf> {
    path.canonicalize().or_else(|error| match path.metadata() {
        Ok(_) => Ok(path.to_owned()),
        Err(_) => Err(error),
    })
}

/// A resolution of a source: the path named, the file naming it, and the
/// file it is, or why none is.
type Resolved = (String, Option<PathBuf>, Result<PathBuf, String>);

/// A config's sources: the file it was, and each attempt to read one in,
/// in order: as it was named, from which file, the file it was, and its
/// text, or why it wasn't there (which a `try` may have caught).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Frozen {
    pub(super) name: String,
    pub(super) text: String,
    pub(super) path: Option<PathBuf>,
    resolved: Vec<Resolved>,
    read: Vec<(PathBuf, Result<String, String>)>,
}

impl Frozen {
    /// The file at `path`, read.
    pub(super) fn file(path: &Path) -> io::Result<Self> {
        let path = identity(path)?;
        let text = read(&path)?;
        Ok(Self {
            name: path.display().to_string(),
            text,
            path: Some(path),
            resolved: Vec::new(),
            read: Vec::new(),
        })
    }

    /// `text`, a source named `name` and no file's.
    #[cfg(test)]
    pub(super) fn text(name: &str, text: &str) -> Self {
        Self {
            name: name.to_owned(),
            text: text.to_owned(),
            path: None,
            resolved: Vec::new(),
            read: Vec::new(),
        }
    }

    /// It, with the files `files` read while it was read.
    pub(super) fn with(self, files: Files) -> Self {
        Self {
            resolved: files.resolved,
            read: files.read,
            ..self
        }
    }
}

/// Where a program's `include` and `require` read files: the file system,
/// relative to the directory fux started in, each file it read kept; a
/// config's sources only, replayed in the order they were read; or none.
pub struct Files {
    cwd: io::Result<PathBuf>,
    /// A config's sources, and how many of its resolutions and reads were
    /// replayed.
    frozen: Option<(Config, usize, usize)>,
    /// No file is read: a key's or a prompt's program.
    sealed: bool,
    pub(super) required: Vec<PathBuf>,
    pub(super) loading: Vec<PathBuf>,
    resolved: Vec<Resolved>,
    read: Vec<(PathBuf, Result<String, String>)>,
    /// Marked before a file that may not read the same again (a pipe) is
    /// read: the command line's program then isn't run again elsewhere.
    pub(super) acted: Option<super::Acted>,
}

impl Default for Files {
    fn default() -> Self {
        Self {
            cwd: std::env::current_dir(),
            frozen: None,
            sealed: false,
            required: Vec::new(),
            loading: Vec::new(),
            resolved: Vec::new(),
            read: Vec::new(),
            acted: None,
        }
    }
}

impl Files {
    /// Only `config`'s sources, as they were read, in order.
    pub(super) fn frozen(config: Config) -> Self {
        Self {
            frozen: Some((config, 0, 0)),
            ..Self::default()
        }
    }

    /// None at all.
    pub(super) fn sealed() -> Self {
        Self {
            sealed: true,
            ..Self::default()
        }
    }

    /// The file `path` names, as one identity: its canonical path.
    pub(super) fn resolve(&mut self, path: &str, from: Option<&Path>) -> Result<PathBuf, String> {
        if self.sealed {
            return Err("a key's or prompt's program reads no files".to_owned());
        }
        if let Some((config, at, _)) = &mut self.frozen {
            let found = config
                .resolved
                .get(*at)
                .filter(|(p, f, _)| p == path && f.as_deref() == from);
            *at = at.saturating_add(1);
            return found.map_or_else(|| Err(unread(path)), |(.., file)| file.clone());
        }
        let file = self.live(path, from);
        // Kept whatever came of it, for a replay to meet the same.
        let kept = (path.to_owned(), from.map(Path::to_owned), file.clone());
        self.resolved.push(kept);
        file
    }

    /// The file `path` names from `from`, on the file system.
    fn live(&self, path: &str, from: Option<&Path>) -> Result<PathBuf, String> {
        let named = Path::new(path);
        let full = if named.is_absolute() {
            named.to_owned()
        } else {
            let base = match from.and_then(Path::parent) {
                Some(parent) => parent,
                None => self
                    .cwd
                    .as_deref()
                    .map_err(|e| format!("the invocation directory is unknown: {e}"))?,
            };
            base.join(named)
        };
        identity(&full).map_err(|e| e.to_string())
    }

    /// The text of the file `resolve` gave: replayed, the next one the
    /// config read.
    pub(super) fn read(&mut self, path: &Path) -> Result<String, String> {
        if let Some((config, _, at)) = &mut self.frozen {
            let found = config.read.get(*at).filter(|(p, _)| p == path);
            *at = at.saturating_add(1);
            let unread = || Err(unread(&path.display().to_string()));
            return found.map_or_else(unread, |(_, text)| text.clone());
        }
        if let Some(acted) = &self.acted
            && !path.metadata().is_ok_and(|m| m.is_file())
        {
            acted.mark();
        }
        let text = read(path).map_err(|e| e.to_string());
        self.read.push((path.to_owned(), text.clone()));
        text
    }
}

fn unread(path: &str) -> String {
    format!("{path}: not read so when the config was")
}
