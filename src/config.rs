//! Typed, validated user configuration. Small on purpose: shell/program default, prefix and
//! bindings, bounded history, clipboard policy and resource limits.

use crate::commands::Action;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::env;
use std::ffi::OsString;
use std::fmt;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

pub const MAX_CONFIG_BYTES: u64 = 1024 * 1024;
pub const MAX_COMMAND_ARGS: usize = 128;
pub const MAX_COMMAND_ARG_BYTES: usize = 4096;
pub const MAX_COMMAND_BYTES: usize = 16 * 1024;
pub const MAX_SCROLLBACK_LINES: u32 = 100_000;
pub const MAX_PANES: usize = crate::view::MAX_PANES;
pub const MAX_TABS: usize = crate::view::MAX_TABS;
pub const MAX_WORKSPACES: usize = 64;

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub struct Config {
    pub prefix: String,
    pub bindings: BTreeMap<String, Action>,
    pub default_command: Command,
    pub clipboard: ClipboardPolicy,
    pub history: HistoryLimits,
    pub limits: Limits,
    pub style: Style,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            prefix: crate::commands::key_name(crate::commands::DEFAULT_PREFIX),
            bindings: default_bindings(),
            default_command: default_shell(),
            clipboard: ClipboardPolicy::Disabled,
            history: HistoryLimits::default(),
            limits: Limits::default(),
            style: Style::default(),
        }
    }
}

impl Config {
    /// Parses a sparse TOML document over the built-in defaults.
    pub fn from_toml(input: &str) -> Result<Self, ConfigError> {
        let patch: ConfigPatch = toml::from_str(input).map_err(ConfigError::Toml)?;
        let candidate = patch.apply_to(Self::default());
        candidate.validate()?;
        Ok(candidate)
    }

    /// Loads a sparse configuration file. A missing file means built-in defaults.
    pub fn load_from_path(path: &Path) -> Result<Self, ConfigError> {
        match fs::File::open(path) {
            Ok(file) => {
                let mut bytes = Vec::new();
                file.take(MAX_CONFIG_BYTES + 1)
                    .read_to_end(&mut bytes)
                    .map_err(|error| ConfigError::Io {
                        path: path.to_owned(),
                        error,
                    })?;
                if bytes.len() as u64 > MAX_CONFIG_BYTES {
                    return invalid(
                        "config file",
                        format!("may use at most {MAX_CONFIG_BYTES} bytes"),
                    );
                }
                let input = String::from_utf8(bytes).map_err(|_| ConfigError::Invalid {
                    field: "config file",
                    reason: "must be UTF-8".to_owned(),
                })?;
                Self::from_toml(&input)
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(ConfigError::Io {
                path: path.to_owned(),
                error,
            }),
        }
    }

    /// Loads the configuration from [`default_path`].
    pub fn load() -> Result<Self, ConfigError> {
        Self::load_from_path(&default_path()?)
    }

    pub fn to_toml_pretty(&self) -> Result<String, ConfigError> {
        toml::to_string_pretty(self).map_err(ConfigError::Serialize)
    }

    pub fn validate(&self) -> Result<(), ConfigError> {
        let prefix = validate_key_notation("prefix", &self.prefix)?;
        if self.bindings.len() > 256 {
            return invalid("bindings", "at most 256 entries are allowed");
        }
        let mut seen = std::collections::BTreeSet::new();
        for key in self.bindings.keys() {
            let byte = validate_key_notation("bindings key", key)?;
            if !seen.insert(byte) {
                return invalid("bindings", "two key names encode the same byte");
            }
            if byte == prefix {
                return invalid("bindings", "a binding cannot equal the prefix key");
            }
        }
        self.default_command.validate("default-command")?;
        self.history.validate()?;
        self.limits.validate()
    }
}

/// Resolves `$XDG_CONFIG_HOME/fux/config.toml`, falling back to `$HOME/.config/fux/config.toml`.
pub fn default_path() -> Result<PathBuf, ConfigError> {
    default_path_from(env::var_os("XDG_CONFIG_HOME"), env::var_os("HOME"))
}

pub fn default_path_from(
    xdg_config_home: Option<OsString>,
    home: Option<OsString>,
) -> Result<PathBuf, ConfigError> {
    let base = xdg_config_home
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .filter(|value| value.is_absolute())
        .or_else(|| {
            home.filter(|value| !value.is_empty())
                .map(PathBuf::from)
                .filter(|value| value.is_absolute())
                .map(|value| value.join(".config"))
        })
        .ok_or(ConfigError::NoConfigHome)?;
    Ok(base.join("fux").join("config.toml"))
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct Command {
    pub argv: Vec<String>,
}

impl Command {
    pub fn new(argv: Vec<String>) -> Result<Self, ConfigError> {
        let command = Self { argv };
        command.validate("command")?;
        Ok(command)
    }

    fn validate(&self, field: &'static str) -> Result<(), ConfigError> {
        if self.argv.is_empty() || self.argv.len() > MAX_COMMAND_ARGS {
            return invalid(
                field,
                format!("argv must contain 1-{MAX_COMMAND_ARGS} entries"),
            );
        }
        let mut total = 0usize;
        for argument in &self.argv {
            if argument.is_empty()
                || argument.len() > MAX_COMMAND_ARG_BYTES
                || argument.contains('\0')
            {
                return invalid(
                    field,
                    "arguments must be non-empty, bounded UTF-8 without NUL",
                );
            }
            total = total.saturating_add(argument.len());
        }
        if total > MAX_COMMAND_BYTES {
            return invalid(
                field,
                format!("argv may use at most {MAX_COMMAND_BYTES} bytes"),
            );
        }
        Ok(())
    }
}

/// One of the sixteen ANSI colours, the terminal's default foreground, or no colour at all.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum StyleColor {
    Black,
    Red,
    Green,
    Yellow,
    Blue,
    Magenta,
    Cyan,
    White,
    BrightBlack,
    BrightRed,
    BrightGreen,
    BrightYellow,
    BrightBlue,
    BrightMagenta,
    BrightCyan,
    BrightWhite,
    #[default]
    Default,
    None,
}

/// Colours of the bar and separators. Defaults are muted and work on dark and light terminals.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields, default)]
pub struct Style {
    /// Workspace name, inactive tabs and the focused pane's `id: title`.
    pub bar: StyleColor,
    /// The current tab (drawn reversed).
    pub tab_active: StyleColor,
    /// Separators not touching the focused pane.
    pub separator: StyleColor,
    /// Separators touching the focused pane (also bold).
    pub separator_focused: StyleColor,
    /// Transient notices in the bar; errors always use red.
    pub notice: StyleColor,
}

impl Default for Style {
    fn default() -> Self {
        Self {
            bar: StyleColor::BrightBlack,
            tab_active: StyleColor::Default,
            separator: StyleColor::BrightBlack,
            separator_focused: StyleColor::Default,
            notice: StyleColor::Yellow,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ClipboardPolicy {
    /// Never write to the enclosing terminal's clipboard.
    #[default]
    Disabled,
    /// Copies and application OSC 52 writes reach the terminal clipboard (bounded, once).
    WriteOnly,
}

impl ClipboardPolicy {
    #[must_use]
    pub const fn writes(self) -> bool {
        matches!(self, Self::WriteOnly)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields, default)]
pub struct HistoryLimits {
    /// Retained history rows per pane.
    pub scrollback_lines: u32,
}

impl Default for HistoryLimits {
    fn default() -> Self {
        Self {
            scrollback_lines: 10_000,
        }
    }
}

impl HistoryLimits {
    fn validate(&self) -> Result<(), ConfigError> {
        if self.scrollback_lines == 0 || self.scrollback_lines > MAX_SCROLLBACK_LINES {
            return invalid(
                "history.scrollback-lines",
                format!("must be 1-{MAX_SCROLLBACK_LINES}"),
            );
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields, default)]
pub struct Limits {
    pub max_panes: usize,
    pub max_tabs: usize,
    pub max_workspaces: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_panes: MAX_PANES,
            max_tabs: MAX_TABS,
            max_workspaces: MAX_WORKSPACES,
        }
    }
}

impl Limits {
    fn validate(&self) -> Result<(), ConfigError> {
        validate_limit("limits.max-panes", self.max_panes, MAX_PANES)?;
        validate_limit("limits.max-tabs", self.max_tabs, MAX_TABS)?;
        validate_limit("limits.max-workspaces", self.max_workspaces, MAX_WORKSPACES)
    }
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct ConfigPatch {
    prefix: Option<String>,
    bindings: Option<BTreeMap<String, Action>>,
    default_command: Option<Command>,
    clipboard: Option<ClipboardPolicy>,
    history: Option<HistoryLimits>,
    limits: Option<Limits>,
    style: Option<Style>,
}

impl ConfigPatch {
    fn apply_to(self, mut config: Config) -> Config {
        if let Some(value) = self.prefix {
            config.prefix = value;
        }
        if let Some(value) = self.bindings {
            config.bindings.extend(value);
        }
        if let Some(value) = self.default_command {
            config.default_command = value;
        }
        if let Some(value) = self.clipboard {
            config.clipboard = value;
        }
        if let Some(value) = self.history {
            config.history = value;
        }
        if let Some(value) = self.limits {
            config.limits = value;
        }
        if let Some(value) = self.style {
            config.style = value;
        }
        config
    }
}

#[derive(Debug)]
pub enum ConfigError {
    NoConfigHome,
    Io {
        path: PathBuf,
        error: std::io::Error,
    },
    Toml(toml::de::Error),
    Serialize(toml::ser::Error),
    Invalid {
        field: &'static str,
        reason: String,
    },
}

impl fmt::Display for ConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoConfigHome => write!(formatter, "neither XDG_CONFIG_HOME nor HOME is set"),
            Self::Io { path, error } => {
                write!(formatter, "failed to read {}: {error}", path.display())
            }
            Self::Toml(error) => write!(formatter, "invalid configuration TOML: {error}"),
            Self::Serialize(error) => {
                write!(formatter, "failed to serialize configuration: {error}")
            }
            Self::Invalid { field, reason } => write!(formatter, "invalid `{field}`: {reason}"),
        }
    }
}

impl std::error::Error for ConfigError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { error, .. } => Some(error),
            Self::Toml(error) => Some(error),
            Self::Serialize(error) => Some(error),
            Self::NoConfigHome | Self::Invalid { .. } => None,
        }
    }
}

fn default_shell() -> Command {
    let shell = default_shell_from(
        env::var_os("SHELL"),
        env::var_os("PREFIX"),
        cfg!(target_os = "android"),
    );
    Command {
        argv: vec![shell, "-l".to_owned()],
    }
}

pub fn default_shell_from(
    shell: Option<OsString>,
    prefix: Option<OsString>,
    android: bool,
) -> String {
    shell
        .filter(|value| !value.is_empty())
        .map(|value| value.to_string_lossy().into_owned())
        .or_else(|| {
            prefix.map(|prefix| {
                PathBuf::from(prefix)
                    .join("bin/sh")
                    .to_string_lossy()
                    .into_owned()
            })
        })
        .unwrap_or_else(|| {
            if android {
                "/system/bin/sh".to_owned()
            } else {
                "/bin/sh".to_owned()
            }
        })
}

fn default_bindings() -> BTreeMap<String, Action> {
    crate::commands::DEFAULT_BINDINGS
        .iter()
        .map(|spec| (crate::commands::key_name(spec.key), spec.action))
        .collect()
}

fn validate_key_notation(field: &'static str, value: &str) -> Result<u8, ConfigError> {
    crate::commands::key_byte(value).map_or_else(
        || {
            invalid(
                field,
                "must encode exactly one byte as a literal byte, `C-x`, `Esc`, `Space` or `DEL`",
            )
        },
        Ok,
    )
}

fn validate_limit(field: &'static str, value: usize, maximum: usize) -> Result<(), ConfigError> {
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
    fn style_table_parses_named_colours_and_rejects_unknown_ones() {
        let config = Config::from_toml(
            "[style]\nbar = \"blue\"\ntab-active = \"none\"\nseparator-focused = \"bright-white\"\n",
        )
        .unwrap_or_default();
        assert_eq!(config.style.bar, StyleColor::Blue);
        assert_eq!(config.style.tab_active, StyleColor::None);
        assert_eq!(config.style.separator_focused, StyleColor::BrightWhite);
        assert_eq!(
            config.style.separator,
            StyleColor::BrightBlack,
            "defaults fill the rest"
        );
        assert!(Config::from_toml("[style]\nbar = \"teal\"\n").is_err());
        assert!(Config::from_toml("[style]\naccent = \"red\"\n").is_err());
    }

    #[test]
    fn defaults_round_trip_and_sparse_documents_merge() {
        let config = Config::default();
        assert!(config.validate().is_ok());
        let text = config.to_toml_pretty().unwrap_or_default();
        let parsed = Config::from_toml(&text).unwrap_or_default();
        assert_eq!(parsed, config);
        let sparse = Config::from_toml("prefix = 'C-b'\n[history]\nscrollback-lines = 5\n")
            .unwrap_or_default();
        assert_eq!(sparse.prefix, "C-b");
        assert_eq!(sparse.history.scrollback_lines, 5);
        assert_eq!(sparse.bindings, config.bindings);
    }

    #[test]
    fn invalid_documents_are_rejected() {
        assert!(Config::from_toml("prefix = 'ab'").is_err());
        assert!(Config::from_toml("[bindings]\n'C-a' = 'detach'").is_err());
        assert!(Config::from_toml("[bindings]\n'x' = 'zoom'").is_err());
        assert!(Config::from_toml("zor-path = '/bin/true'").is_err());
        assert!(Config::from_toml("[hints]\ndelay-ms = 0").is_err());
        assert!(Config::from_toml("[history]\nscrollback-lines = 0").is_err());
        assert!(Config::from_toml("[limits]\nmax-panes = 100000").is_err());
        assert!(Config::from_toml("default-command = { argv = [] }").is_err());
        assert!(Config::from_toml("clipboard = 'read-write'").is_err());
    }

    #[test]
    fn shell_default_prefers_env_then_platform() {
        assert_eq!(
            default_shell_from(Some("/usr/bin/fish".into()), None, false),
            "/usr/bin/fish"
        );
        assert_eq!(default_shell_from(Some("".into()), None, false), "/bin/sh");
        assert_eq!(default_shell_from(None, None, true), "/system/bin/sh");
        assert_eq!(
            default_shell_from(None, Some("/data/usr".into()), true),
            "/data/usr/bin/sh"
        );
    }

    #[test]
    fn config_path_prefers_xdg_then_home() {
        assert_eq!(
            default_path_from(Some("/x".into()), Some("/h".into())).ok(),
            Some(PathBuf::from("/x/fux/config.toml"))
        );
        assert_eq!(
            default_path_from(Some("".into()), Some("/h".into())).ok(),
            Some(PathBuf::from("/h/.config/fux/config.toml"))
        );
        assert!(default_path_from(None, None).is_err());
    }
}
