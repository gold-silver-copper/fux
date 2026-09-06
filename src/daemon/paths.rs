//! Private runtime/state locations for one user.

use std::env;
use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DaemonPaths {
    pub runtime_dir: PathBuf,
    pub state_dir: PathBuf,
    pub manager_socket: PathBuf,
    pub descriptors_dir: PathBuf,
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
            runtime_dir,
            state_dir,
        })
    }

    pub fn prepare(&self) -> Result<(), PathError> {
        private_dir(&self.runtime_dir)?;
        private_dir(&self.state_dir)?;
        private_dir(&self.descriptors_dir)
    }

    pub fn descriptor(&self, name: &str) -> Result<PathBuf, PathError> {
        crate::ids::validate_workspace_name(name).map_err(|_| PathError::UnsafeName)?;
        Ok(self.descriptors_dir.join(format!("{name}.json")))
    }

    pub fn attach_socket(&self, name: &str) -> Result<PathBuf, PathError> {
        crate::ids::validate_workspace_name(name).map_err(|_| PathError::UnsafeName)?;
        Ok(self.runtime_dir.join(format!("{name}.attach.sock")))
    }

    pub fn control_socket(&self, name: &str) -> Result<PathBuf, PathError> {
        crate::ids::validate_workspace_name(name).map_err(|_| PathError::UnsafeName)?;
        Ok(self.runtime_dir.join(format!("{name}.sock")))
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

#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum PathError {
    #[error("XDG_RUNTIME_DIR (or HOME on macOS) must be set to an absolute path")]
    MissingRuntime,
    #[error("XDG_STATE_HOME or HOME must be set to an absolute path")]
    MissingState,
    #[error("unsafe workspace name")]
    UnsafeName,
    #[error("{} must be a private directory owned by this user", .0.display())]
    UnsafeDirectory(PathBuf),
    #[error("{0}")]
    Io(String),
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
            use std::os::unix::fs::DirBuilderExt as _;
            fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(path)
                .map_err(|error| PathError::Io(error.to_string()))?;
        }
        Err(error) => return Err(PathError::Io(error.to_string())),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_derive_from_runtime_and_state_homes() {
        let paths = DaemonPaths::from_env(
            Some("/run/user/1".into()),
            Some("/home/u/.local/state".into()),
            Some("/home/u".into()),
        )
        .unwrap_or_else(|_| DaemonPaths {
            runtime_dir: PathBuf::new(),
            state_dir: PathBuf::new(),
            manager_socket: PathBuf::new(),
            descriptors_dir: PathBuf::new(),
        });
        assert_eq!(
            paths.manager_socket,
            PathBuf::from("/run/user/1/fux/manager.sock")
        );
        assert_eq!(
            paths.attach_socket("default").ok(),
            Some(PathBuf::from("/run/user/1/fux/default.attach.sock"))
        );
        assert_eq!(
            paths.control_socket("default").ok(),
            Some(PathBuf::from("/run/user/1/fux/default.sock"))
        );
        assert!(paths.descriptor("../x").is_err());
        assert!(DaemonPaths::from_env(Some("relative".into()), None, None).is_err());
    }
}
