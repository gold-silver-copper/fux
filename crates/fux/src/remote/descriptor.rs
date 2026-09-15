//! `<runtime_dir>/<server>.brp.json`: how clients find and authorise against a server. Written
//! 0600 by the server at `PostStartup`, removed when the App drops.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use bevy_ecs::prelude::*;
use serde::{Deserialize, Serialize};

/// Descriptors are tiny; anything larger is not ours.
pub const MAX_DESCRIPTOR_BYTES: u64 = 64 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Descriptor {
    /// Server instance nonce; every mutating request carries it.
    pub instance: String,
    pub pid: u32,
    pub http: Endpoint,
    /// Attachment stream endpoint, `null` until the attachment listener published one.
    pub attach: Option<AttachDescriptor>,
    /// The server token: every capability over every workspace.
    pub token: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Endpoint {
    pub host: String,
    pub port: u16,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AttachDescriptor {
    pub host: String,
    pub port: u16,
    pub token: String,
}

#[derive(Debug)]
pub enum DescriptorError {
    Io {
        path: PathBuf,
        error: std::io::Error,
    },
    Json {
        path: PathBuf,
        error: serde_json::Error,
    },
    TooLarge {
        path: PathBuf,
    },
    /// The file is not a regular file owned by this user with mode 0600.
    Insecure {
        path: PathBuf,
    },
}

impl core::fmt::Display for DescriptorError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Io { path, error } => write!(f, "{}: {error}", path.display()),
            Self::Json { path, error } => write!(f, "{}: {error}", path.display()),
            Self::TooLarge { path } => write!(f, "{}: descriptor too large", path.display()),
            Self::Insecure { path } => {
                write!(f, "{}: not a private regular file", path.display())
            }
        }
    }
}

impl std::error::Error for DescriptorError {}

pub fn descriptor_path(runtime_dir: &Path, server_name: &str) -> PathBuf {
    runtime_dir.join(format!("{server_name}.brp.json"))
}

/// Reads a descriptor without following symlinks and refuses files other users can read.
pub fn read_descriptor(path: &Path) -> Result<Descriptor, DescriptorError> {
    let io = |error| DescriptorError::Io {
        path: path.to_path_buf(),
        error,
    };
    let metadata = fs::symlink_metadata(path).map_err(io)?;
    if !metadata.is_file() || metadata.permissions().mode() & 0o077 != 0 {
        return Err(DescriptorError::Insecure {
            path: path.to_path_buf(),
        });
    }
    if metadata.len() > MAX_DESCRIPTOR_BYTES {
        return Err(DescriptorError::TooLarge {
            path: path.to_path_buf(),
        });
    }
    let bytes = fs::read(path).map_err(io)?;
    serde_json::from_slice(&bytes).map_err(|error| DescriptorError::Json {
        path: path.to_path_buf(),
        error,
    })
}

/// Writes atomically (0600 temp file + rename) so a reader never sees a partial descriptor.
pub fn write_descriptor(path: &Path, descriptor: &Descriptor) -> Result<(), DescriptorError> {
    let io = |error| DescriptorError::Io {
        path: path.to_path_buf(),
        error,
    };
    let bytes = serde_json::to_vec_pretty(descriptor).map_err(|error| DescriptorError::Json {
        path: path.to_path_buf(),
        error,
    })?;
    let temporary = path.with_extension(format!("tmp-{}", descriptor.pid));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&temporary)
        .map_err(io)?;
    let written = (|| {
        file.write_all(&bytes)?;
        file.sync_all()?;
        fs::set_permissions(&temporary, fs::Permissions::from_mode(0o600))?;
        fs::rename(&temporary, path)
    })();
    if let Err(error) = written {
        let _ = fs::remove_file(&temporary);
        return Err(io(error));
    }
    Ok(())
}

/// Removes the descriptor when the App (and with it the World) drops, which happens before the
/// process exits by any path that runs destructors; `OnExit(ShuttingDown)` would be too late
/// for a runner that returns `AppExit` from inside the last update.
#[derive(Resource, Debug)]
pub struct DescriptorGuard(pub PathBuf);

impl Drop for DescriptorGuard {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Descriptor {
        Descriptor {
            instance: "abc".into(),
            pid: 42,
            http: Endpoint {
                host: "127.0.0.1".into(),
                port: 1,
            },
            attach: None,
            token: "t".into(),
        }
    }

    #[test]
    fn round_trips_with_private_mode_and_guard_removes() {
        let dir = tempfile::tempdir().unwrap();
        let path = descriptor_path(dir.path(), "x");
        write_descriptor(&path, &sample()).unwrap();
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(read_descriptor(&path).unwrap(), sample());
        drop(DescriptorGuard(path.clone()));
        assert!(!path.exists());
    }

    #[test]
    fn refuses_world_readable_descriptor() {
        let dir = tempfile::tempdir().unwrap();
        let path = descriptor_path(dir.path(), "x");
        write_descriptor(&path, &sample()).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(matches!(
            read_descriptor(&path),
            Err(DescriptorError::Insecure { .. })
        ));
    }
}
