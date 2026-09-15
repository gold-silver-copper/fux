//! `fux.toml`, in the old vocabulary: `prefix`, `default-command`, `[history]`, `[limits]`,
//! `[final]`. Every key is optional over the defaults; unknown keys are errors; the result is
//! the `Limits` resource plus the default command and prefix.

use std::io::Read;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::model::{Limits, MAX_LEAVES, MAX_NODES_PER_WORKSPACE};

pub const MAX_CONFIG_BYTES: u64 = 1024 * 1024;
pub const MAX_SCROLLBACK_LINES: usize = 100_000;
/// Four hours.
pub const MAX_FINAL_RETAIN_MS: u64 = 4 * 60 * 60 * 1000;
pub const MAX_WORKSPACES: usize = 64;
pub const MAX_VIEWERS: usize = 64;
pub const MAX_COMMAND_ARGS: usize = 128;
pub const MAX_COMMAND_ARG_BYTES: usize = 4096;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields, default)]
pub struct Config {
    /// Prefix key in the old notation (`C-b`); interpreted by the viewer.
    pub prefix: String,
    pub default_command: Command,
    pub history: History,
    pub limits: LimitsSection,
    #[serde(rename = "final")]
    pub final_records: FinalRecords,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            prefix: "C-b".into(),
            default_command: Command::default(),
            history: History::default(),
            limits: LimitsSection::default(),
            final_records: FinalRecords::default(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct Command {
    pub argv: Vec<String>,
}

impl Default for Command {
    fn default() -> Self {
        Self {
            argv: vec![crate::lifecycle::default_shell()],
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields, default)]
pub struct History {
    pub scrollback_lines: usize,
}

impl Default for History {
    fn default() -> Self {
        Self {
            scrollback_lines: Limits::default().scrollback_lines,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields, default)]
pub struct LimitsSection {
    pub max_panes: usize,
    pub max_nodes: usize,
    pub max_workspaces: usize,
    pub max_viewers: usize,
}

impl Default for LimitsSection {
    fn default() -> Self {
        let limits = Limits::default();
        Self {
            max_panes: limits.panes_per_workspace,
            max_nodes: limits.nodes_per_workspace,
            max_workspaces: limits.workspaces,
            max_viewers: limits.viewers,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields, default)]
pub struct FinalRecords {
    pub retain_ms: u64,
}

impl Default for FinalRecords {
    fn default() -> Self {
        Self {
            retain_ms: Limits::default().final_retain_ms,
        }
    }
}

#[derive(Debug)]
pub enum ConfigError {
    Io(PathBuf, std::io::Error),
    Toml(toml::de::Error),
    Invalid { field: &'static str, reason: String },
}

impl core::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Io(path, error) => write!(f, "{}: {error}", path.display()),
            Self::Toml(error) => write!(f, "config: {error}"),
            Self::Invalid { field, reason } => write!(f, "config: {field} {reason}"),
        }
    }
}

impl core::error::Error for ConfigError {}

impl Config {
    /// Parses a sparse TOML document over the defaults.
    pub fn from_toml(input: &str) -> Result<Self, ConfigError> {
        let config: Self = toml::from_str(input).map_err(ConfigError::Toml)?;
        config.validate()?;
        Ok(config)
    }

    /// A missing file means defaults.
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        let io = |error| ConfigError::Io(path.to_owned(), error);
        match std::fs::File::open(path) {
            Ok(file) => {
                let mut bytes = Vec::new();
                file.take(MAX_CONFIG_BYTES + 1)
                    .read_to_end(&mut bytes)
                    .map_err(io)?;
                if bytes.len() as u64 > MAX_CONFIG_BYTES {
                    return invalid("file", format!("may use at most {MAX_CONFIG_BYTES} bytes"));
                }
                let input = String::from_utf8(bytes).map_err(|_| ConfigError::Invalid {
                    field: "file",
                    reason: "must be UTF-8".into(),
                })?;
                Self::from_toml(&input)
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(io(error)),
        }
    }

    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.prefix.is_empty() {
            return invalid("prefix", "must not be empty");
        }
        let argv = &self.default_command.argv;
        if argv.is_empty() || argv.len() > MAX_COMMAND_ARGS {
            return invalid(
                "default-command.argv",
                format!("needs 1-{MAX_COMMAND_ARGS} entries"),
            );
        }
        if argv
            .iter()
            .any(|a| a.is_empty() || a.len() > MAX_COMMAND_ARG_BYTES || a.contains('\0'))
        {
            return invalid(
                "default-command.argv",
                "has an empty, oversize or NUL argument",
            );
        }
        let lines = self.history.scrollback_lines;
        if lines == 0 || lines > MAX_SCROLLBACK_LINES {
            return invalid(
                "history.scrollback-lines",
                format!("must be 1-{MAX_SCROLLBACK_LINES}"),
            );
        }
        let retain = self.final_records.retain_ms;
        if retain == 0 || retain > MAX_FINAL_RETAIN_MS {
            return invalid(
                "final.retain-ms",
                format!("must be 1-{MAX_FINAL_RETAIN_MS}"),
            );
        }
        bounded("limits.max-panes", self.limits.max_panes, MAX_LEAVES)?;
        bounded(
            "limits.max-nodes",
            self.limits.max_nodes,
            MAX_NODES_PER_WORKSPACE,
        )?;
        bounded(
            "limits.max-workspaces",
            self.limits.max_workspaces,
            MAX_WORKSPACES,
        )?;
        bounded("limits.max-viewers", self.limits.max_viewers, MAX_VIEWERS)
    }

    /// The `Limits` resource this configuration describes.
    pub fn limits(&self) -> Limits {
        Limits {
            panes_per_workspace: self.limits.max_panes,
            nodes_per_workspace: self.limits.max_nodes,
            workspaces: self.limits.max_workspaces,
            viewers: self.limits.max_viewers,
            scrollback_lines: self.history.scrollback_lines,
            final_retain_ms: self.final_records.retain_ms,
            ..Limits::default()
        }
        .clamped()
    }
}

fn bounded(field: &'static str, value: usize, maximum: usize) -> Result<(), ConfigError> {
    if value == 0 || value > maximum {
        return invalid(field, format!("must be 1-{maximum}"));
    }
    Ok(())
}

fn invalid<T>(field: &'static str, reason: impl Into<String>) -> Result<T, ConfigError> {
    Err(ConfigError::Invalid {
        field,
        reason: reason.into(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sparse_document_overlays_defaults() {
        let config = Config::from_toml(
            "prefix = \"C-a\"\n[history]\nscrollback-lines = 50\n[final]\nretain-ms = 10\n",
        )
        .unwrap();
        assert_eq!(config.prefix, "C-a");
        let limits = config.limits();
        assert_eq!(limits.scrollback_lines, 50);
        assert_eq!(limits.final_retain_ms, 10);
        assert_eq!(
            limits.panes_per_workspace,
            Limits::default().panes_per_workspace
        );
    }

    #[test]
    fn unknown_keys_and_out_of_range_values_are_rejected() {
        assert!(Config::from_toml("bogus = 1").is_err());
        assert!(Config::from_toml("[limits]\nmax-panes = 100000").is_err());
        assert!(Config::from_toml("[history]\nscrollback-lines = 0").is_err());
        assert!(Config::from_toml("[default-command]\nargv = []").is_err());
    }

    #[test]
    fn missing_file_is_default() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            Config::load(&dir.path().join("none.toml")).unwrap(),
            Config::default()
        );
    }
}
