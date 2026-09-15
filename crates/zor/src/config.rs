//! `zor.toml`: `fux-server`, `[limits]`, `[archive]`. Every key is optional over the defaults;
//! unknown keys are errors; the result is the `Limits` resource plus the fux server name.

use std::io::Read;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::model::{Limits, MAX_JOURNAL_BYTES};

pub const MAX_CONFIG_BYTES: u64 = 1024 * 1024;
/// A year, the largest archive age accepted.
pub const MAX_ARCHIVE_AFTER_MS: u64 = 366 * 24 * 60 * 60 * 1000;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields, default)]
pub struct Config {
    /// The fux server whose descriptor zor reads (`FUX_BRP` overrides the path).
    pub fux_server: String,
    pub limits: LimitsSection,
    pub archive: Archive,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            fux_server: "default".into(),
            limits: LimitsSection::default(),
            archive: Archive::default(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields, default)]
pub struct LimitsSection {
    pub journal_bytes: usize,
    pub event_log_entries: usize,
    pub tokens: usize,
}

impl Default for LimitsSection {
    fn default() -> Self {
        let limits = Limits::default();
        Self {
            journal_bytes: limits.journal_bytes,
            event_log_entries: limits.event_log_entries,
            tokens: limits.tokens,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields, default)]
pub struct Archive {
    /// Closed tasks older than this move into a dated archive snapshot.
    pub after_ms: u64,
}

impl Default for Archive {
    fn default() -> Self {
        Self {
            after_ms: Limits::default().archive_after_ms,
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
        if self.fux_server.is_empty() || self.fux_server.contains('/') {
            return invalid("fux-server", "must be a non-empty server name");
        }
        let bytes = self.limits.journal_bytes;
        if bytes == 0 || bytes > MAX_JOURNAL_BYTES {
            return invalid(
                "limits.journal-bytes",
                format!("must be 1-{MAX_JOURNAL_BYTES}"),
            );
        }
        if self.limits.event_log_entries == 0 || self.limits.tokens == 0 {
            return invalid("limits", "event-log-entries and tokens must be positive");
        }
        let after = self.archive.after_ms;
        if after == 0 || after > MAX_ARCHIVE_AFTER_MS {
            return invalid(
                "archive.after-ms",
                format!("must be 1-{MAX_ARCHIVE_AFTER_MS}"),
            );
        }
        Ok(())
    }

    pub fn limits(&self) -> Limits {
        Limits {
            journal_bytes: self.limits.journal_bytes,
            event_log_entries: self.limits.event_log_entries,
            tokens: self.limits.tokens,
            archive_after_ms: self.archive.after_ms,
            ..Limits::default()
        }
        .clamped()
    }
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
    fn sparse_documents_fill_defaults_and_unknown_keys_fail() {
        let config = Config::from_toml("[archive]\nafter-ms = 1000\n").unwrap();
        assert_eq!(config.archive.after_ms, 1000);
        assert_eq!(config.fux_server, "default");
        assert!(Config::from_toml("bogus = 1").is_err());
        assert!(Config::from_toml("[limits]\njournal-bytes = 0").is_err());
    }
}
