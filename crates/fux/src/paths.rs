//! Per-user private locations (prompt 3.9/3.12): descriptors under the runtime directory,
//! configuration under `XDG_CONFIG_HOME`, logs and sessions under `XDG_STATE_HOME`.

use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Paths {
    /// `$XDG_RUNTIME_DIR/fux` (fallback `$TMPDIR/fux-<uid>`): `<server>.brp.json` descriptors.
    pub runtime_dir: PathBuf,
    /// `$XDG_CONFIG_HOME/fux` (fallback `$HOME/.config/fux`): `fux.toml`.
    pub config_dir: PathBuf,
    /// `$XDG_STATE_HOME/fux` (fallback `$HOME/.local/state/fux`): logs, sessions.
    pub state_dir: PathBuf,
}

#[derive(Debug)]
pub enum PathError {
    /// Neither `XDG_RUNTIME_DIR` nor a temp directory resolves to an absolute path.
    MissingRuntime,
    /// Neither the XDG variable nor `HOME` resolves to an absolute path.
    MissingHome,
    /// The directory exists but is not a private directory owned by this user.
    Unsafe(PathBuf),
    Io(PathBuf, std::io::Error),
}

impl core::fmt::Display for PathError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::MissingRuntime => {
                write!(f, "XDG_RUNTIME_DIR or TMPDIR must be an absolute path")
            }
            Self::MissingHome => write!(f, "XDG_*_HOME or HOME must be an absolute path"),
            Self::Unsafe(path) => write!(
                f,
                "{} must be a private directory owned by this user",
                path.display()
            ),
            Self::Io(path, error) => write!(f, "{}: {error}", path.display()),
        }
    }
}

impl core::error::Error for PathError {}

impl Paths {
    pub fn discover() -> Result<Self, PathError> {
        Self::from_env(
            std::env::var_os("XDG_RUNTIME_DIR"),
            std::env::var_os("TMPDIR"),
            std::env::var_os("XDG_CONFIG_HOME"),
            std::env::var_os("XDG_STATE_HOME"),
            std::env::var_os("HOME"),
        )
    }

    pub fn from_env(
        runtime: Option<OsString>,
        tmp: Option<OsString>,
        config: Option<OsString>,
        state: Option<OsString>,
        home: Option<OsString>,
    ) -> Result<Self, PathError> {
        let runtime_dir = match absolute(runtime) {
            Some(dir) => dir.join("fux"),
            None => absolute(tmp)
                .or_else(|| Some(PathBuf::from("/tmp")))
                .map(|dir| dir.join(format!("fux-{}", nix::unistd::getuid().as_raw())))
                .ok_or(PathError::MissingRuntime)?,
        };
        let home = absolute(home);
        let config_dir = absolute(config)
            .or_else(|| home.as_ref().map(|h| h.join(".config")))
            .ok_or(PathError::MissingHome)?
            .join("fux");
        let state_dir = absolute(state)
            .or_else(|| home.as_ref().map(|h| h.join(".local/state")))
            .ok_or(PathError::MissingHome)?
            .join("fux");
        Ok(Self {
            runtime_dir,
            config_dir,
            state_dir,
        })
    }

    /// Creates the runtime and state directories privately (0700) and verifies ownership.
    pub fn prepare(&self) -> Result<(), PathError> {
        private_dir(&self.runtime_dir)?;
        private_dir(&self.state_dir)
    }

    pub fn descriptor(&self, server: &str) -> PathBuf {
        self.runtime_dir.join(format!("{server}.brp.json"))
    }

    pub fn config_file(&self) -> PathBuf {
        self.config_dir.join("fux.toml")
    }

    pub fn log_file(&self, server: &str) -> PathBuf {
        self.state_dir.join(format!("{server}.log"))
    }
}

fn absolute(value: Option<OsString>) -> Option<PathBuf> {
    value
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
}

/// Creates `path` with mode 0700 (parents included) and refuses a directory that is not owned
/// by this user or is group/world accessible.
pub fn private_dir(path: &Path) -> Result<(), PathError> {
    let io = |error| PathError::Io(path.to_owned(), error);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(io)?;
    }
    match fs::DirBuilder::new().mode(0o700).create(path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(io(error)),
    }
    let meta = fs::symlink_metadata(path).map_err(io)?;
    let uid = nix::unistd::getuid().as_raw();
    if !meta.is_dir() || meta.uid() != uid || meta.permissions().mode() & 0o077 != 0 {
        return Err(PathError::Unsafe(path.to_owned()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_falls_back_to_tmp_with_uid() {
        let paths = Paths::from_env(
            None,
            Some("/var/tmp".into()),
            None,
            None,
            Some("/home/u".into()),
        )
        .unwrap();
        let uid = nix::unistd::getuid().as_raw();
        assert_eq!(
            paths.runtime_dir,
            PathBuf::from(format!("/var/tmp/fux-{uid}"))
        );
        assert_eq!(paths.config_dir, PathBuf::from("/home/u/.config/fux"));
        assert_eq!(paths.state_dir, PathBuf::from("/home/u/.local/state/fux"));
    }

    #[test]
    fn relative_values_are_ignored() {
        let paths = Paths::from_env(
            Some("relative".into()),
            Some("/t".into()),
            Some("/c".into()),
            Some("/s".into()),
            None,
        )
        .unwrap();
        assert_eq!(paths.runtime_dir.parent(), Some(Path::new("/t")));
        assert_eq!(paths.config_dir, PathBuf::from("/c/fux"));
        assert!(Paths::from_env(None, None, None, None, None).is_err());
    }

    #[test]
    fn private_dir_rejects_group_readable() {
        let dir = tempfile::tempdir().unwrap();
        let loose = dir.path().join("loose");
        fs::create_dir(&loose).unwrap();
        fs::set_permissions(&loose, fs::Permissions::from_mode(0o750)).unwrap();
        assert!(matches!(private_dir(&loose), Err(PathError::Unsafe(_))));
        let fresh = dir.path().join("fresh/nested");
        private_dir(&fresh).unwrap();
        assert_eq!(
            fs::metadata(&fresh).unwrap().permissions().mode() & 0o777,
            0o700
        );
    }
}
