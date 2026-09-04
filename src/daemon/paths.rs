use std::env;
use std::ffi::OsString;
use std::fmt;
use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DaemonPaths {
    pub runtime_dir: PathBuf,
    pub state_dir: PathBuf,
    pub manager_socket: PathBuf,
    pub descriptors_dir: PathBuf,
    pub keys_dir: PathBuf,
}

impl DaemonPaths {
    pub fn discover() -> Result<Self, PathError> {
        Self::from_env(
            env::var_os("XDG_RUNTIME_DIR"),
            env::var_os("XDG_STATE_HOME"),
            env::var_os("HOME"),
        )
    }

    pub fn from_env(
        runtime: Option<OsString>,
        state: Option<OsString>,
        home: Option<OsString>,
    ) -> Result<Self, PathError> {
        let runtime = absolute(runtime)
            .or_else(|| macos_runtime_fallback(home.as_ref()))
            .ok_or(PathError::MissingRuntime)?;
        let state = absolute(state)
            .or_else(|| absolute(home).map(|path| path.join(".local/state")))
            .ok_or(PathError::MissingState)?;
        let runtime_dir = runtime.join("fux");
        let state_dir = state.join("fux");
        Ok(Self {
            manager_socket: runtime_dir.join("manager.sock"),
            descriptors_dir: runtime_dir.join("workspaces"),
            keys_dir: state_dir.join("keys"),
            runtime_dir,
            state_dir,
        })
    }

    pub fn prepare(&self) -> Result<(), PathError> {
        private_dir(&self.runtime_dir)?;
        private_dir(&self.state_dir)?;
        private_dir(&self.descriptors_dir)?;
        private_dir(&self.keys_dir)
    }

    pub fn descriptor(&self, name: &str) -> Result<PathBuf, PathError> {
        validate_workspace_name(name)?;
        Ok(self.descriptors_dir.join(format!("{name}.json")))
    }

    pub fn key(&self, name: &str) -> Result<PathBuf, PathError> {
        validate_workspace_name(name)?;
        Ok(self.keys_dir.join(format!("{name}.key")))
    }
}

#[cfg(target_os = "macos")]
fn macos_runtime_fallback(home: Option<&OsString>) -> Option<PathBuf> {
    absolute(home.cloned()).map(|path| path.join("Library/Caches/fux-runtime"))
}

#[cfg(not(target_os = "macos"))]
fn macos_runtime_fallback(_: Option<&OsString>) -> Option<PathBuf> {
    None
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PathError {
    MissingRuntime,
    MissingState,
    UnsafeName,
    UnsafeDirectory(PathBuf),
    Io(String),
}

impl fmt::Display for PathError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for PathError {}

pub fn validate_workspace_name(name: &str) -> Result<(), PathError> {
    if name.is_empty()
        || name.len() > 64
        || name == "."
        || name == ".."
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        return Err(PathError::UnsafeName);
    }
    Ok(())
}

fn absolute(value: Option<OsString>) -> Option<PathBuf> {
    value
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
}

fn private_dir(path: &Path) -> Result<(), PathError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if !metadata.is_dir()
                || metadata.file_type().is_symlink()
                || metadata.permissions().mode() & 0o077 != 0
            {
                return Err(PathError::UnsafeDirectory(path.to_owned()));
            }
            if let Some(parent) = path.parent()
                && let Ok(parent_metadata) = fs::metadata(parent)
                && metadata.uid() != parent_metadata.uid()
            {
                return Err(PathError::UnsafeDirectory(path.to_owned()));
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            fs::create_dir_all(path).map_err(|error| PathError::Io(error.to_string()))?;
            fs::set_permissions(path, fs::Permissions::from_mode(0o700))
                .map_err(|error| PathError::Io(error.to_string()))?;
        }
        Err(error) => return Err(PathError::Io(error.to_string())),
    }
    Ok(())
}
