//! Workspace descriptors: which server instance serves a workspace and where to attach.

use super::{DaemonPaths, PathError};
use serde::{Deserialize, Serialize};
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

pub const MAX_DESCRIPTOR_BYTES: u64 = 64 * 1024;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Descriptor {
    pub name: String,
    pub pid: u32,
    pub instance_nonce: String,
    pub socket_path: PathBuf,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ManagerIdentity {
    pub pid: u32,
    pub instance_nonce: String,
}

impl ManagerIdentity {
    pub fn current() -> std::io::Result<Self> {
        Ok(Self {
            pid: std::process::id(),
            instance_nonce: random_token()?,
        })
    }
}

impl Descriptor {
    pub fn validate(&self) -> Result<(), DescriptorError> {
        crate::ids::validate_workspace_name(&self.name)
            .map_err(|_| DescriptorError::Path(PathError::UnsafeName))?;
        if self.pid == 0
            || !safe_token(&self.instance_nonce, 128)
            || !self.socket_path.is_absolute()
        {
            return Err(DescriptorError::Invalid);
        }
        Ok(())
    }

    pub fn belongs_to(&self, manager: &ManagerIdentity) -> bool {
        self.pid == manager.pid && self.instance_nonce == manager.instance_nonce
    }
}

#[derive(Debug, thiserror::Error)]
pub enum DescriptorError {
    #[error(transparent)]
    Path(#[from] PathError),
    #[error("{}: {error}", path.display())]
    Io {
        path: PathBuf,
        #[source]
        error: std::io::Error,
    },
    #[error("invalid workspace descriptor")]
    Invalid,
    #[error("descriptor JSON: {0}")]
    Json(#[source] serde_json::Error),
    #[error("descriptor exceeds size limit")]
    TooLarge,
}

fn io_at(path: &Path) -> impl FnOnce(std::io::Error) -> DescriptorError + '_ {
    move |error| DescriptorError::Io {
        path: path.to_owned(),
        error,
    }
}

/// Opens `path` without following symlinks and reads at most one byte past the size limit.
fn read_bounded(path: &Path) -> std::io::Result<(fs::Metadata, Vec<u8>)> {
    let mut options = OpenOptions::new();
    options.read(true).custom_flags(nix::libc::O_NOFOLLOW);
    let file = options.open(path)?;
    let metadata = file.metadata()?;
    let mut bytes = Vec::new();
    file.take(MAX_DESCRIPTOR_BYTES + 1)
        .read_to_end(&mut bytes)?;
    Ok((metadata, bytes))
}

pub fn read_descriptor(
    path: &Path,
    expected_name: &str,
    manager: &ManagerIdentity,
) -> Result<Descriptor, DescriptorError> {
    let parent = path.parent().ok_or(DescriptorError::Invalid)?;
    let parent_metadata = fs::metadata(parent).map_err(io_at(parent))?;
    crate::ids::validate_workspace_name(expected_name)
        .map_err(|_| DescriptorError::Path(PathError::UnsafeName))?;
    let (opened, bytes) = read_bounded(path).map_err(io_at(path))?;
    if !opened.is_file()
        || opened.permissions().mode() & 0o077 != 0
        || opened.len() > MAX_DESCRIPTOR_BYTES
        || opened.uid() != parent_metadata.uid()
    {
        return Err(DescriptorError::Invalid);
    }
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
    let mut file = options.open(&temporary).map_err(io_at(&temporary))?;
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

/// Removes a descriptor only when it is a regular, owner-matching file.
pub fn remove_descriptor(paths: &DaemonPaths, name: &str) -> Result<(), DescriptorError> {
    let path = paths.descriptor(name).map_err(DescriptorError::Path)?;
    let owner = fs::metadata(&paths.descriptors_dir)
        .map_err(io_at(&paths.descriptors_dir))?
        .uid();
    match fs::symlink_metadata(&path) {
        Ok(metadata)
            if metadata.file_type().is_file()
                && !metadata.file_type().is_symlink()
                && metadata.uid() == owner =>
        {
            fs::remove_file(&path).map_err(|error| DescriptorError::Io { path, error })
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Ok(_) => Err(DescriptorError::Invalid),
        Err(error) => Err(DescriptorError::Io { path, error }),
    }
}

/// Removes regular, owner-matching descriptor files that do not belong to this server instance.
/// Symlinks and other file types are reported rather than followed or removed.
pub fn recover_stale_descriptors(
    paths: &DaemonPaths,
    manager: &ManagerIdentity,
) -> Result<usize, DescriptorError> {
    paths.prepare().map_err(DescriptorError::Path)?;
    let dir = &paths.descriptors_dir;
    let owner = fs::metadata(dir).map_err(io_at(dir))?;
    let mut removed = 0usize;
    for entry in fs::read_dir(dir).map_err(io_at(dir))? {
        let path = entry.map_err(io_at(dir))?.path();
        if path.extension().and_then(|value| value.to_str()) != Some("json") {
            continue;
        }
        let metadata = fs::symlink_metadata(&path).map_err(io_at(&path))?;
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

pub fn random_token() -> std::io::Result<String> {
    let mut bytes = [0_u8; 16];
    fs::File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}
