//! Local service locations, independent of task/process authority.
use anyhow::{Result, ensure};
use std::path::{Path, PathBuf};

pub(crate) struct Endpoint<'a> {
    runtime: &'a Path,
}

impl<'a> Endpoint<'a> {
    pub fn new(runtime: &'a Path) -> Self {
        Self { runtime }
    }

    pub fn manager(&self) -> PathBuf {
        self.runtime.join("manager.sock")
    }

    pub fn workspace(&self, name: &str) -> Result<PathBuf> {
        ensure!(valid_name(name), "invalid fux workspace name");
        Ok(self.runtime.join(format!("{name}.sock")))
    }
}

pub(crate) fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && !matches!(name, "." | "..")
        && !name
            .chars()
            .any(|c| c.is_control() || c == '/' || c == '\\')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workspace_paths_cannot_escape_the_runtime() -> Result<()> {
        let endpoint = Endpoint::new(Path::new("/private/runtime"));
        for name in ["", ".", "..", "../other", "a/b", "a\\b", "a\n"] {
            assert!(endpoint.workspace(name).is_err(), "accepted {name:?}");
        }
        assert_eq!(
            endpoint.workspace("my workspace")?,
            Path::new("/private/runtime/my workspace.sock")
        );
        Ok(())
    }
}
