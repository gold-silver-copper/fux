//! Local storage identity for native thread recreation. Never reads credentials
//! or rollout contents; the provider remains responsible for interpreting them.
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Storage {
    pub directory: PathBuf,
    pub device: u64,
    pub inode: u64,
    pub rollout: PathBuf,
}

impl Storage {
    /// Pin the nearest existing storage ancestor. Providers may allocate dated
    /// paths before creating their directories; no provider files are created.
    pub fn capture(rollout: &Path) -> Result<Self> {
        ensure!(
            rollout.is_absolute(),
            "native rollout path must be absolute"
        );
        let mut ancestor = rollout.parent().context("native rollout parent missing")?;
        let directory = loop {
            match ancestor.canonicalize() {
                Ok(directory) => break directory,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    ancestor = ancestor
                        .parent()
                        .context("native storage ancestor missing")?;
                }
                Err(error) => return Err(error.into()),
            }
        };
        let relative = rollout.strip_prefix(ancestor)?;
        ensure!(
            relative
                .components()
                .all(|part| matches!(part, std::path::Component::Normal(_))),
            "native rollout path contains traversal"
        );
        let metadata = fs::symlink_metadata(&directory)?;
        ensure!(
            metadata.is_dir() && metadata.uid() == nix::unistd::geteuid().as_raw(),
            "native rollout directory is not owned"
        );
        let identity = Self {
            rollout: directory.join(relative),
            directory,
            device: metadata.dev(),
            inode: metadata.ino(),
        };
        identity.verify()?;
        Ok(identity)
    }

    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.directory.is_absolute()
                && self.rollout.is_absolute()
                && self
                    .rollout
                    .strip_prefix(&self.directory)
                    .is_ok_and(|relative| !relative.as_os_str().is_empty()
                        && relative
                            .components()
                            .all(|part| matches!(part, std::path::Component::Normal(_))))
                && self.rollout.file_name().is_some()
                && self
                    .directory
                    .to_str()
                    .is_some_and(|path| path.len() <= 4096 && !path.chars().any(char::is_control))
                && self
                    .rollout
                    .to_str()
                    .is_some_and(|path| path.len() <= 4096 && !path.chars().any(char::is_control)),
            "invalid retained native storage identity"
        );
        Ok(())
    }

    pub fn verify(&self) -> Result<()> {
        self.validate()?;
        let metadata =
            fs::symlink_metadata(&self.directory).context("native rollout directory missing")?;
        ensure!(
            metadata.is_dir()
                && metadata.uid() == nix::unistd::geteuid().as_raw()
                && metadata.dev() == self.device
                && metadata.ino() == self.inode,
            "native storage directory changed; recreation is disabled"
        );
        let mut path = self.directory.clone();
        for part in self.rollout.strip_prefix(&self.directory)?.components() {
            path.push(part);
            match fs::symlink_metadata(&path) {
                Ok(file) => ensure!(
                    (if path == self.rollout {
                        file.is_file()
                    } else {
                        file.is_dir()
                    }) && file.uid() == nix::unistd::geteuid().as_raw(),
                    "native rollout path is redirected or not owned"
                ),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => break,
                Err(error) => return Err(error.into()),
            }
        }
        Ok(())
    }

    pub fn verify_resumed(&self, reported: &Path) -> Result<()> {
        self.verify()?;
        ensure!(
            Self::capture(reported)?.rollout == self.rollout,
            "native resumed storage namespace changed"
        );
        Ok(())
    }

    /// A reported allocation alone is not a persisted session. Check this
    /// before stopping the current app-server for explicit recreation.
    pub fn require_materialized(&self) -> Result<()> {
        self.verify()?;
        let file = fs::symlink_metadata(&self.rollout)
            .context("native rollout has not been materialized; recreation is unavailable")?;
        ensure!(
            file.is_file(),
            "native rollout is not a materialized regular file"
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_file_updates_preserve_namespace_but_redirects_and_replacement_fail() -> Result<()> {
        let root = tempfile::tempdir()?;
        let directory = root.path().join("sessions");
        fs::create_dir(&directory)?;
        let rollout = directory.join("thread.jsonl");
        let storage = Storage::capture(&rollout)?;
        assert!(storage.require_materialized().is_err());
        fs::write(&rollout, b"provider owns these bytes")?;
        storage.require_materialized()?;
        storage.verify_resumed(&rollout)?;
        // The provider may replace its file atomically in the same namespace.
        let replacement = directory.join("new");
        fs::write(&replacement, b"new provider contents")?;
        fs::rename(replacement, &rollout)?;
        storage.verify_resumed(&rollout)?;
        assert!(
            storage
                .verify_resumed(&directory.join("different.jsonl"))
                .is_err()
        );
        fs::remove_file(&rollout)?;
        std::os::unix::fs::symlink(root.path().join("elsewhere"), &rollout)?;
        assert!(storage.verify().is_err());
        fs::rename(&directory, root.path().join("old-sessions"))?;
        fs::create_dir(&directory)?;
        assert!(storage.verify().is_err());
        Ok(())
    }

    #[test]
    fn allocated_paths_pin_existing_ancestors_and_reject_invalid_paths() -> Result<()> {
        let root = tempfile::tempdir()?;
        assert!(Storage::capture(Path::new("thread.jsonl")).is_err());
        let rollout = root.path().join("missing/thread.jsonl");
        let storage = Storage::capture(&rollout)?;
        assert_eq!(storage.directory, root.path().canonicalize()?);
        fs::create_dir(root.path().join("missing"))?;
        fs::write(&rollout, b"provider data")?;
        storage.verify_resumed(&rollout)?;
        fs::rename(root.path().join("missing"), root.path().join("moved"))?;
        std::os::unix::fs::symlink(root.path().join("moved"), root.path().join("missing"))?;
        assert!(storage.verify().is_err());
        assert!(Storage::capture(&root.path().join("bad\nname")).is_err());
        Ok(())
    }
}
