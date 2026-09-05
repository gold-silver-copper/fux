use super::{DaemonPaths, MAX_DESCRIPTOR_BYTES, PathError, validate_workspace_name};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Descriptor {
    pub name: String,
    pub pid: u32,
    pub instance_nonce: String,
    pub socket_path: PathBuf,
    pub protocol: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ManagerIdentity {
    pub pid: u32,
    pub instance_nonce: String,
}

impl Descriptor {
    pub fn validate(&self) -> Result<(), DescriptorError> {
        validate_workspace_name(&self.name).map_err(DescriptorError::Path)?;
        if self.pid == 0
            || !safe_token(&self.instance_nonce, 128)
            || !self.socket_path.is_absolute()
            || self.protocol != crate::local::VERSION
        {
            return Err(DescriptorError::Invalid);
        }
        Ok(())
    }

    pub fn belongs_to(&self, manager: &ManagerIdentity) -> bool {
        self.pid == manager.pid && self.instance_nonce == manager.instance_nonce
    }
}

#[derive(Debug)]
pub enum DescriptorError {
    Path(PathError),
    Io {
        path: PathBuf,
        error: std::io::Error,
    },
    Invalid,
    Json(serde_json::Error),
    TooLarge,
}
impl fmt::Display for DescriptorError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for DescriptorError {}

pub fn read_descriptor(
    path: &Path,
    expected_name: &str,
    manager: &ManagerIdentity,
) -> Result<Descriptor, DescriptorError> {
    let parent = path.parent().ok_or(DescriptorError::Invalid)?;
    let parent_metadata = fs::metadata(parent).map_err(|error| DescriptorError::Io {
        path: parent.to_owned(),
        error,
    })?;
    validate_workspace_name(expected_name).map_err(DescriptorError::Path)?;
    let mut bytes = Vec::new();
    let mut options = OpenOptions::new();
    options.read(true).custom_flags(nix::libc::O_NOFOLLOW);
    let file = options.open(path).map_err(|error| DescriptorError::Io {
        path: path.to_owned(),
        error,
    })?;
    let opened = file.metadata().map_err(|error| DescriptorError::Io {
        path: path.to_owned(),
        error,
    })?;
    if !opened.is_file()
        || opened.permissions().mode() & 0o077 != 0
        || opened.len() > MAX_DESCRIPTOR_BYTES
        || opened.uid() != parent_metadata.uid()
    {
        return Err(DescriptorError::Invalid);
    }
    file.take(MAX_DESCRIPTOR_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| DescriptorError::Io {
            path: path.to_owned(),
            error,
        })?;
    if bytes.len() as u64 > MAX_DESCRIPTOR_BYTES {
        return Err(DescriptorError::TooLarge);
    }
    let descriptor: Descriptor = serde_json::from_slice(&bytes).map_err(DescriptorError::Json)?;
    descriptor.validate()?;
    if descriptor.name != expected_name || !descriptor.belongs_to(manager) {
        return Err(DescriptorError::Invalid);
    }
    Ok(descriptor)
}

pub fn write_descriptor(
    paths: &DaemonPaths,
    descriptor: &Descriptor,
) -> Result<PathBuf, DescriptorError> {
    descriptor.validate()?;
    paths.prepare().map_err(DescriptorError::Path)?;
    let target = paths
        .descriptor(&descriptor.name)
        .map_err(DescriptorError::Path)?;
    let temporary = target.with_extension(format!(
        "tmp-{}-{}",
        descriptor.pid, descriptor.instance_nonce
    ));
    let bytes = serde_json::to_vec(descriptor).map_err(DescriptorError::Json)?;
    if bytes.len() as u64 > MAX_DESCRIPTOR_BYTES {
        return Err(DescriptorError::TooLarge);
    }
    let mut options = OpenOptions::new();
    options.write(true).create_new(true).mode(0o600);
    let mut file = options
        .open(&temporary)
        .map_err(|error| DescriptorError::Io {
            path: temporary.clone(),
            error,
        })?;
    let result = (|| {
        file.write_all(&bytes)?;
        file.sync_all()?;
        fs::set_permissions(&temporary, fs::Permissions::from_mode(0o600))?;
        fs::rename(&temporary, &target)?;
        if let Some(parent) = target.parent() {
            fs::File::open(parent)?.sync_all()?;
        }
        Ok::<_, std::io::Error>(())
    })();
    if let Err(error) = result {
        let _ = fs::remove_file(&temporary);
        return Err(DescriptorError::Io {
            path: target,
            error,
        });
    }
    Ok(target)
}

/// Removes only regular, owner-matching descriptor files that do not belong to the elected
/// manager instance. Symlinks and other file types are reported rather than followed or removed.
pub fn recover_stale_descriptors(
    paths: &DaemonPaths,
    manager: &ManagerIdentity,
) -> Result<usize, DescriptorError> {
    paths.prepare().map_err(DescriptorError::Path)?;
    let owner = fs::metadata(&paths.descriptors_dir).map_err(|error| DescriptorError::Io {
        path: paths.descriptors_dir.clone(),
        error,
    })?;
    let mut removed = 0usize;
    for entry in fs::read_dir(&paths.descriptors_dir).map_err(|error| DescriptorError::Io {
        path: paths.descriptors_dir.clone(),
        error,
    })? {
        let entry = entry.map_err(|error| DescriptorError::Io {
            path: paths.descriptors_dir.clone(),
            error,
        })?;
        let path = entry.path();
        if path.extension().and_then(|value| value.to_str()) != Some("json") {
            continue;
        }
        let metadata = fs::symlink_metadata(&path).map_err(|error| DescriptorError::Io {
            path: path.clone(),
            error,
        })?;
        if !metadata.file_type().is_file()
            || metadata.file_type().is_symlink()
            || metadata.uid() != owner.uid()
        {
            return Err(DescriptorError::Invalid);
        }
        let Some(name) = path.file_stem().and_then(|value| value.to_str()) else {
            return Err(DescriptorError::Invalid);
        };
        if read_descriptor(&path, name, manager).is_err() {
            fs::remove_file(&path).map_err(|error| DescriptorError::Io { path, error })?;
            removed = removed.saturating_add(1);
        }
    }
    Ok(removed)
}

fn safe_token(value: &str, max: usize) -> bool {
    !value.is_empty()
        && value.len() <= max
        && !value.chars().any(char::is_whitespace)
        && !value.contains('\0')
}
