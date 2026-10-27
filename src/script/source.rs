//! Filesystem admission for compile-time sources. Every image has its own
//! loader and captured cwd; replay needs neither.
use super::{Failed, read_failed};
use fuxth::{Loader, Source, SourceImage, SourceLimits, Token};
use std::io::{self, Read};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

struct Files {
    cwd: Option<PathBuf>,
}

impl Files {
    fn new() -> Self {
        Self {
            cwd: std::env::current_dir().ok(),
        }
    }
}

impl Loader for Files {
    fn resolve(&mut self, path: &str, from: Option<&Path>) -> Result<PathBuf, String> {
        let path = Path::new(path);
        let full = if path.is_absolute() {
            path.to_owned()
        } else {
            from.and_then(Path::parent)
                .or(self.cwd.as_deref())
                .ok_or("the invocation directory is unknown")?
                .join(path)
        };
        full.canonicalize().map_err(|e| e.to_string())
    }

    fn read(&mut self, path: &Path, remaining: usize) -> Result<String, String> {
        read(open(path).map_err(|e| e.to_string())?, remaining).map_err(|e| e.to_string())
    }
}

pub(super) fn open(path: &Path) -> io::Result<std::fs::File> {
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(fuxix::file::NONBLOCK | fuxix::file::NOCTTY)
        .open(path)?;
    if !file.metadata()?.is_file() {
        return Err(io::Error::other("not a regular source file"));
    }
    Ok(file)
}

fn read(file: std::fs::File, remaining: usize) -> io::Result<String> {
    let mut bytes = Vec::new();
    file.take(
        u64::try_from(remaining)
            .unwrap_or(u64::MAX)
            .saturating_add(1),
    )
    .read_to_end(&mut bytes)?;
    if bytes.len() > remaining {
        return Err(io::Error::other("source byte limit exceeded"));
    }
    String::from_utf8(bytes).map_err(|_| io::Error::other("not UTF-8 text"))
}

pub(super) fn root(path: &Path) -> io::Result<(PathBuf, std::fs::File)> {
    let path = path.canonicalize()?;
    let file = open(&path)?;
    Ok((path, file))
}

pub(super) fn file(path: &Path) -> Result<SourceImage, Failed> {
    let (path, file) =
        root(path).map_err(|e| read_failed(&path.display().to_string(), &e.to_string()))?;
    opened_file(path, file)
}

pub(super) fn opened_file(path: PathBuf, file: std::fs::File) -> Result<SourceImage, Failed> {
    let name = path.display().to_string();
    let text = read(file, super::SOURCE_MAX).map_err(|e| read_failed(&name, &e.to_string()))?;
    Ok(SourceImage::load(
        Source {
            name,
            text: text.into(),
            path: Some(path),
        },
        &mut Files::new(),
        SourceLimits::default(),
    )?)
}

pub(super) fn text(name: &str, text: &str) -> Result<SourceImage, Failed> {
    Ok(SourceImage::load(
        Source {
            name: name.into(),
            text: text.into(),
            path: None,
        },
        &mut Files::new(),
        SourceLimits::default(),
    )?)
}

pub(super) fn arguments(tokens: &[Token]) -> Result<SourceImage, Failed> {
    Ok(SourceImage::arguments(
        tokens,
        &mut Files::new(),
        SourceLimits::default(),
    )?)
}

#[cfg(test)]
mod tests {
    use super::*;
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

    fn run(image: &SourceImage) -> Result<String, Box<dyn std::error::Error>> {
        let mut machine = crate::script::compile(&image.tokens()).map_err(|e| e.to_string())?;
        let mut out = Vec::new();
        let mut host =
            crate::script::Host::new(crate::protocol::Context::default(), None, &mut out);
        assert!(matches!(
            machine.run(&mut host, 1000)?,
            fuxth::Outcome::Finished
        ));
        Ok(String::from_utf8(out)?)
    }

    #[test]
    fn a_diamond_runs_each_canonical_file_once_and_keeps_its_sources()
    -> Result<(), Box<dyn std::error::Error>> {
        let f = Fixture::new()?;
        let root = f.write("main.fux", "require sub/a.fux require b.fux answer print")?;
        f.write("sub/a.fux", "require '../shared file.fux'")?;
        f.write("b.fux", "require alias.fux")?;
        let shared = f.write("shared file.fux", "str once print define answer 41 end")?;
        std::os::unix::fs::symlink(&shared, f.0.join("alias.fux"))?;
        let image = file(&root).map_err(|e| e.to_string())?;
        assert_eq!(run(&image)?, "once\n41\n");
        std::fs::remove_file(&shared)?;
        assert_eq!(run(&image)?, "once\n41\n");
        // Parsing operands never become imports; includes work even in an
        // unexecuted branch, because they are resolved before execution.
        assert_eq!(
            run(&text("literal", "str require print").map_err(|e| e.to_string())?)?,
            "require\n"
        );
        assert!(text("branch", "false if require /no/such/fux-source then").is_err());
        Ok(())
    }

    #[test]
    fn import_failures_name_the_chain_before_any_effect() -> Result<(), Box<dyn std::error::Error>>
    {
        let f = Fixture::new()?;
        let root = f.write("main.fux", "str before print\nrequire child.fux")?;
        f.write("child.fux", "\nrequire main.fux")?;
        let cycle = file(&root).err().ok_or("cycle accepted")?.to_string();
        assert!(cycle.contains("main.fux:2 ->"), "{cycle}");
        assert!(cycle.contains("child.fux:2: require"), "{cycle}");
        assert!(cycle.contains("cycle"), "{cycle}");
        for broken in [
            "\nno-such-word",
            "\nstr 'open",
            "\n1 true add",
            "\nrequire missing.fux",
        ] {
            f.write("child.fux", broken)?;
            let error = file(&root)
                .and_then(|image| crate::script::compile(&image.tokens()).map(drop))
                .err()
                .ok_or("bad source accepted")?
                .to_string();
            assert!(error.contains("main.fux:2 ->"), "{error}");
            assert!(error.contains("child.fux:2"), "{error}");
        }
        Ok(())
    }

    #[test]
    fn aggregate_limits_include_the_root_and_refuse_before_growth()
    -> Result<(), Box<dyn std::error::Error>> {
        let f = Fixture::new()?;
        let root = f.write("main.fux", "require child.fux")?;
        f.write("child.fux", "1 2")?;
        let source = Source {
            name: root.display().to_string(),
            text: "require child.fux".into(),
            path: Some(root.canonicalize()?),
        };
        let exact = SourceLimits {
            bytes: 20,
            files: 2,
            depth: 1,
            tokens: 4,
        };
        // The source text is 17 + 3 bytes, and all four input tokens count.
        assert!(SourceImage::load(source.clone(), &mut Files::new(), exact).is_ok());
        for limits in [
            SourceLimits { bytes: 19, ..exact },
            SourceLimits { files: 1, ..exact },
            SourceLimits { depth: 0, ..exact },
            SourceLimits { tokens: 3, ..exact },
        ] {
            assert!(SourceImage::load(source.clone(), &mut Files::new(), limits).is_err());
        }
        assert!(fuxth::token::source_limited("t", "1 2 # comment\n", 2).is_ok());
        assert!(fuxth::token::source_limited("t", "1 2 3", 2).is_err());
        assert!(fuxth::token::source_limited("t", "''", 0).is_err());
        let mut unknown = Files { cwd: None };
        assert!(unknown.resolve("relative.fux", None).is_err());
        assert_eq!(
            unknown.resolve(root.to_str().ok_or("nontext fixture")?, None)?,
            root.canonicalize()?
        );
        Ok(())
    }

    #[test]
    fn source_admission_refuses_fifos_devices_and_directories()
    -> Result<(), Box<dyn std::error::Error>> {
        let f = Fixture::new()?;
        let fifo = f.0.join("pipe");
        assert!(
            std::process::Command::new("mkfifo")
                .arg(&fifo)
                .status()?
                .success()
        );
        for path in [&fifo, &f.0, Path::new("/dev/zero")] {
            assert!(file(path).is_err(), "{} admitted", path.display());
        }
        let root = f.write("main.fux", "require pipe")?;
        assert!(file(&root).is_err());
        Ok(())
    }

    #[test]
    fn client_definitions_use_the_frozen_image_until_a_new_load()
    -> Result<(), Box<dyn std::error::Error>> {
        let f = Fixture::new()?;
        let root = f.write("main.fux", "require child.fux")?;
        f.write("child.fux", "define answer 41 end")?;
        let old = crate::script::run_file(&root, None, None).map_err(|e| e.to_string())?;
        f.write("child.fux", "define answer 42 end")?;
        let new = crate::script::run_file(&root, None, None).map_err(|e| e.to_string())?;
        for (keys, expected) in [(&old, "41\n"), (&new, "42\n"), (&old, "41\n")] {
            let mut machine = crate::script::compiled(
                keys.config.as_ref(),
                &fuxth::token::source("key", "answer print")?,
            )?;
            let mut out = Vec::new();
            let mut host =
                crate::script::Host::new(crate::protocol::Context::default(), None, &mut out);
            assert!(matches!(
                machine.run(&mut host, 1000)?,
                fuxth::Outcome::Finished
            ));
            assert_eq!(String::from_utf8(out)?, expected);
        }
        Ok(())
    }
}
