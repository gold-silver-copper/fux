//! The files a program reads (`include`, `require`, `--file` and the
//! config): any readable file, a FIFO or `<(…)` among them, relative to the
//! file naming it, else to where fux started. A config's are kept as they
//! were read, so its definitions are read again from the same text.
use super::{Config, Failed};
use fuxth::Token;
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
    name: String,
    text: String,
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

    pub(super) fn tokens(&self) -> Result<Vec<Token>, Failed> {
        Ok(fuxth::token::source(&self.name, &self.text)?)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::TestResult;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Result<Self, Box<dyn std::error::Error>> {
            static NEXT: AtomicUsize = AtomicUsize::new(0);
            let dir = std::env::var_os("TMPDIR")
                .map(PathBuf::from)
                .unwrap_or(std::env::current_dir()?.join("target"));
            let dir = dir.join(format!(
                "require-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir_all(&dir)?;
            Ok(Self(dir))
        }
        fn write(&self, name: &str, text: &str) -> io::Result<PathBuf> {
            let path = self.0.join(name);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&path, text)?;
            Ok(path)
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// The program in the file at `path`, read and run as `fux --file`
    /// runs it: what it printed, or why it failed.
    fn run(path: &Path) -> Result<String, Box<dyn std::error::Error>> {
        let path = path.canonicalize()?;
        let tokens = fuxth::token::source(&path.display().to_string(), &read(&path)?)?;
        let mut machine = crate::script::base(None)?;
        machine.read(tokens, Some(path));
        let mut out = Vec::new();
        let mut host =
            crate::script::Host::new(fux_wire::protocol::Context::default(), None, &mut out);
        let ran = machine.run(&mut host, u64::MAX);
        drop(host);
        let failed = |e| crate::script::Failed::from(e).to_string();
        match ran.map_err(failed)? {
            fuxth::Outcome::Finished => Ok(String::from_utf8(out)?),
            other @ (fuxth::Outcome::Suspended(_) | fuxth::Outcome::Paused) => {
                Err(format!("{other:?}").into())
            }
        }
    }

    #[test]
    fn a_diamond_runs_each_canonical_file_once() -> TestResult {
        let f = Fixture::new()?;
        let root = f.write("main.fux", "require sub/a.fux require b.fux answer print")?;
        f.write("sub/a.fux", "require '../shared file.fux'")?;
        f.write("b.fux", "require alias.fux")?;
        let shared = f.write("shared file.fux", "str once print define answer 41 end")?;
        std::os::unix::fs::symlink(&shared, f.0.join("alias.fux"))?;
        assert_eq!(run(&root)?, "once\n41\n");
        // A word read as text is never a file; `require` reads its file
        // as it is read, in a branch too.
        let literal = f.write("literal.fux", "str require print")?;
        assert_eq!(run(&literal)?, "require\n");
        let branch = f.write("branch.fux", "false if require /no/such/fux-source then")?;
        assert!(run(&branch).is_err());
        Ok(())
    }

    /// A chain of requires, each file requiring the next, reads in order
    /// on a thread whose stack holds a few dozen calls: the sources being
    /// read are the machine's data, so no chain overflows the stack.
    #[test]
    fn a_long_chain_of_requires_reads_on_a_small_stack() -> TestResult {
        let f = Fixture::new()?;
        let root = f.write("f0.fux", "str first print require f1.fux")?;
        for i in 1..1000 {
            f.write(&format!("f{i}.fux"), &format!("require f{}.fux", i + 1))?;
        }
        f.write("f1000.fux", "str last print")?;
        let printed = std::thread::Builder::new()
            .stack_size(128 << 10)
            .spawn(move || run(&root).map_err(|e| e.to_string()))?
            .join()
            .map_err(|_| "reading overflowed its stack")??;
        assert_eq!(printed, "first\nlast\n");
        Ok(())
    }

    #[test]
    fn import_failures_name_the_chain() -> TestResult {
        let f = Fixture::new()?;
        let root = f.write("main.fux", "str before print\nrequire child.fux")?;
        f.write("child.fux", "\nrequire main.fux")?;
        let cycle = run(&root).err().ok_or("cycle accepted")?.to_string();
        assert!(cycle.contains("main.fux:2 ->"), "{cycle}");
        assert!(cycle.contains("child.fux:2: require"), "{cycle}");
        assert!(cycle.contains("cycle"), "{cycle}");
        for broken in [
            "\nno-such-word",
            "\nstr 'open",
            "\nrequire missing.fux",
            "\n1 true add",
        ] {
            f.write("child.fux", broken)?;
            let error = run(&root).err().ok_or("bad source accepted")?.to_string();
            assert!(error.contains("main.fux:2 ->"), "{error}");
            assert!(error.contains("child.fux:2"), "{error}");
        }
        Ok(())
    }

    #[test]
    fn unknown_invocation_directories_keep_their_cause() -> TestResult {
        let f = Fixture::new()?;
        let root = f.write("main.fux", "require child.fux")?;
        f.write("child.fux", "1")?;
        for kind in [io::ErrorKind::NotFound, io::ErrorKind::PermissionDenied] {
            let cause = io::Error::from(kind);
            let said = cause.to_string();
            let mut unknown = Files {
                cwd: Err(cause),
                ..Files::default()
            };
            let error = unknown
                .resolve("relative.fux", None)
                .err()
                .ok_or("unknown cwd accepted")?;
            assert!(error.contains(&said), "{error}");
            assert_eq!(
                unknown.resolve(root.to_str().ok_or("nontext fixture")?, None)?,
                root.canonicalize()?
            );
            assert_eq!(
                unknown.resolve("child.fux", Some(&root))?,
                f.0.join("child.fux").canonicalize()?
            );
        }
        Ok(())
    }

    /// Any file that reads is a source: a FIFO, as `<(…)` gives, is read
    /// to its end; a directory doesn't read.
    #[test]
    fn any_readable_file_is_a_source() -> TestResult {
        let f = Fixture::new()?;
        let fifo = f.0.join("pipe");
        assert!(
            std::process::Command::new("mkfifo")
                .arg(&fifo)
                .status()?
                .success()
        );
        let writing = fifo.clone();
        let writer = std::thread::spawn(move || std::fs::write(writing, "define piped 7 end"));
        let root = f.write("main.fux", "require pipe piped print")?;
        let printed = run(&root);
        writer.join().map_err(|_| "the writer failed")??;
        assert_eq!(printed?, "7\n");
        assert!(run(&f.0).is_err());
        let directory = f.write("dir.fux", &format!("require {}", f.0.display()))?;
        assert!(run(&directory).is_err());
        Ok(())
    }

    /// A config's reads are replayed in the order they were made: a file
    /// read twice, its text different each time, replays both texts.
    #[test]
    fn a_frozen_config_replays_its_reads_in_order() -> TestResult {
        let file = PathBuf::from("/c/child.fux");
        let config = Frozen {
            resolved: vec![
                ("child.fux".into(), None, Ok(file.clone())),
                ("child.fux".into(), None, Ok(file.clone())),
            ],
            read: vec![
                (file.clone(), Ok("define answer 41 end".into())),
                (file.clone(), Ok("define answer 42 end".into())),
            ],
            ..Frozen::text("config", "include child.fux include child.fux")
        };
        let mut files = Files::frozen(std::sync::Arc::new(config));
        for text in ["define answer 41 end", "define answer 42 end"] {
            let path = files.resolve("child.fux", None)?;
            assert_eq!(files.read(&path)?, text);
        }
        assert!(
            files.resolve("child.fux", None).is_err(),
            "read no more than it did"
        );
        Ok(())
    }

    /// A lookup the config caught failing fails again as it is replayed,
    /// so the next one reads what the config read next.
    #[test]
    fn a_caught_failed_lookup_replays_as_it_failed() -> TestResult {
        let f = Fixture::new()?;
        let config = "define optional\n\
            str require try tick include execute catch 2drop end\n\
            end\n\
            optional missing.fux\n\
            include child.fux\n";
        let root = f.write("main.fux", config)?;
        f.write("child.fux", "define answer 41 end")?;
        let state = crate::script::run_file(&root)?;
        let mut machine = crate::script::compiled(
            state.config.as_ref(),
            &fuxth::token::source("key", "answer print")?,
        )?;
        let mut out = Vec::new();
        let mut host =
            crate::script::Host::new(fux_wire::protocol::Context::default(), None, &mut out);
        assert!(matches!(
            machine.run(&mut host, 1000)?,
            fuxth::Outcome::Finished
        ));
        assert_eq!(String::from_utf8(out)?, "41\n");
        Ok(())
    }

    #[test]
    fn client_definitions_use_the_frozen_sources_until_a_new_load() -> TestResult {
        let f = Fixture::new()?;
        let root = f.write("main.fux", "require child.fux")?;
        f.write("child.fux", "define answer 41 end")?;
        let old = crate::script::run_file(&root)?;
        f.write("child.fux", "define answer 42 end")?;
        let new = crate::script::run_file(&root)?;
        for (keys, expected) in [(&old, "41\n"), (&new, "42\n"), (&old, "41\n")] {
            let mut machine = crate::script::compiled(
                keys.config.as_ref(),
                &fuxth::token::source("key", "answer print")?,
            )?;
            let mut out = Vec::new();
            let mut host =
                crate::script::Host::new(fux_wire::protocol::Context::default(), None, &mut out);
            assert!(matches!(
                machine.run(&mut host, 1000)?,
                fuxth::Outcome::Finished
            ));
            assert_eq!(String::from_utf8(out)?, expected);
        }
        Ok(())
    }
}
