//! Per-user private locations: the descriptor under `<runtime>/zor/`, configuration and rule
//! bundles under `XDG_CONFIG_HOME/zor`, the journal and archives under `XDG_STATE_HOME/zor`.
//! fux's descriptor is found the same way (`FUX_BRP` overrides).

use std::ffi::OsString;
use std::path::{Path, PathBuf};

pub use fux::paths::{PathError, private_dir};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Paths {
    /// `$XDG_RUNTIME_DIR/zor` (fallback `$TMPDIR/zor-<uid>`): `<server>.brp.json`.
    pub runtime_dir: PathBuf,
    /// `$XDG_CONFIG_HOME/zor`: `zor.toml`, rule bundles, plugin manifests.
    pub config_dir: PathBuf,
    /// `$XDG_STATE_HOME/zor`: `journal.scn.ron`, `archive/`, logs.
    pub state_dir: PathBuf,
    /// fux's runtime directory, for its descriptors.
    pub fux_runtime_dir: PathBuf,
}

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
        let fux = fux::paths::Paths::from_env(runtime, tmp, config, state, home)?;
        Ok(Self::beside(&fux))
    }

    /// zor's directories next to fux's (`.../fux` → `.../zor`).
    pub fn beside(fux: &fux::paths::Paths) -> Self {
        let sibling = |dir: &Path| {
            dir.file_name().map_or_else(
                || dir.join("zor"),
                |name| {
                    let name = name.to_string_lossy().replacen("fux", "zor", 1);
                    dir.with_file_name(name)
                },
            )
        };
        Self {
            runtime_dir: sibling(&fux.runtime_dir),
            config_dir: sibling(&fux.config_dir),
            state_dir: sibling(&fux.state_dir),
            fux_runtime_dir: fux.runtime_dir.clone(),
        }
    }

    /// Creates the runtime and state directories privately (0700) and verifies ownership.
    pub fn prepare(&self) -> Result<(), PathError> {
        private_dir(&self.runtime_dir)?;
        private_dir(&self.state_dir)?;
        private_dir(&self.archive_dir())
    }

    pub fn descriptor(&self, server: &str) -> PathBuf {
        fux::remote::descriptor::descriptor_path(&self.runtime_dir, server)
    }

    /// fux's descriptor: `FUX_BRP` when set, else `<fux runtime>/<server>.brp.json`.
    pub fn fux_descriptor(&self, server: &str) -> PathBuf {
        std::env::var_os("FUX_BRP")
            .filter(|v| !v.is_empty())
            .map_or_else(
                || fux::remote::descriptor::descriptor_path(&self.fux_runtime_dir, server),
                PathBuf::from,
            )
    }

    pub fn config_file(&self) -> PathBuf {
        self.config_dir.join("zor.toml")
    }

    pub fn journal_file(&self) -> PathBuf {
        self.state_dir.join("journal.scn.ron")
    }

    pub fn archive_dir(&self) -> PathBuf {
        self.state_dir.join("archive")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zor_lives_beside_fux() {
        let paths = Paths::from_env(
            Some("/run/user/1".into()),
            None,
            Some("/home/u/.config".into()),
            Some("/home/u/.local/state".into()),
            Some("/home/u".into()),
        )
        .unwrap();
        assert_eq!(paths.runtime_dir, PathBuf::from("/run/user/1/zor"));
        assert_eq!(paths.fux_runtime_dir, PathBuf::from("/run/user/1/fux"));
        assert_eq!(paths.config_dir, PathBuf::from("/home/u/.config/zor"));
        assert_eq!(
            paths.journal_file(),
            PathBuf::from("/home/u/.local/state/zor/journal.scn.ron")
        );
    }
}
