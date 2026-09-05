//! Typed, validated user configuration.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

pub const MAX_PREFIX_BYTES: usize = 16;
pub const MAX_CONFIG_BYTES: u64 = 1024 * 1024;
pub const MAX_BINDINGS: usize = 128;
pub const MAX_BINDING_KEY_BYTES: usize = 32;
pub const MAX_COMMAND_ARGS: usize = 128;
pub const MAX_COMMAND_ARG_BYTES: usize = 4096;
pub const MAX_COMMAND_BYTES: usize = 16 * 1024;
pub const MAX_HOOKS: usize = 32;
pub const MAX_REMOTE_ALLOW_IDS: usize = 256;
pub const MAX_REMOTE_ALLOW_ID_BYTES: usize = 512;
pub const MAX_SCROLLBACK_LINES: u32 = 100_000;
/// Matches the control protocol's pre-encoding capture ceiling.
pub const MAX_CAPTURE_BYTES: usize = 128 * 1024;
pub const MAX_RESOURCE_UNITS: usize = 256 * 1024 * 1024;
pub const MAX_PANES: usize = 256;
pub const MAX_TABS: usize = 64;
pub const MAX_POPUPS: usize = 32;
pub const MAX_STATUS_SEGMENTS: usize = 128;
pub const MAX_TOTAL_CELLS: usize = 262_144;

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub struct Config {
    pub prefix: String,
    pub bindings: BTreeMap<String, Binding>,
    pub default_command: Command,
    pub zor_path: PathBuf,
    pub clipboard: ClipboardPolicy,
    pub notifications: NotificationPolicy,
    pub history: HistoryLimits,
    pub resources: ResourceLimits,
    pub remote_allow_ids: Vec<String>,
    /// Bind workspace transport to loopback/direct sockets without relay discovery.
    pub local_network: bool,
    pub hooks: Vec<Hook>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            prefix: crate::commands::key_name(crate::commands::DEFAULT_PREFIX),
            bindings: default_bindings(),
            default_command: default_shell(),
            zor_path: PathBuf::from("zor"),
            clipboard: ClipboardPolicy::Disabled,
            notifications: NotificationPolicy::default(),
            history: HistoryLimits::default(),
            resources: ResourceLimits::default(),
            remote_allow_ids: Vec::new(),
            local_network: false,
            hooks: Vec::new(),
        }
    }
}

impl Config {
    /// Parses a sparse TOML document over the built-in defaults.
    pub fn from_toml(input: &str) -> Result<Self, ConfigError> {
        Self::default().merge_toml(input)
    }

    /// Applies a sparse TOML document to this configuration and validates the candidate.
    /// The receiver is unchanged when parsing or validation fails.
    pub fn merge_toml(&self, input: &str) -> Result<Self, ConfigError> {
        let patch: ConfigPatch = toml::from_str(input).map_err(ConfigError::Toml)?;
        let candidate = patch.apply_to(self.clone());
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
        if self.bindings.len() > MAX_BINDINGS {
            return invalid(
                "bindings",
                format!("at most {MAX_BINDINGS} entries are allowed"),
            );
        }
        let mut seen = std::collections::BTreeSet::new();
        for (key, binding) in &self.bindings {
            let byte = validate_key_notation("bindings key", key)?;
            if !seen.insert(byte) {
                return invalid("bindings", "two key names encode the same byte");
            }
            if byte == prefix {
                return invalid("bindings", "a binding cannot equal the prefix key");
            }
            if let Binding::External { external } = binding {
                external.validate("bindings external command")?;
            }
        }
        self.default_command.validate("default-command")?;
        validate_path("zor-path", &self.zor_path)?;
        self.history.validate()?;
        self.resources.validate()?;
        if self.remote_allow_ids.len() > MAX_REMOTE_ALLOW_IDS {
            return invalid(
                "remote-allow-ids",
                format!("at most {MAX_REMOTE_ALLOW_IDS} endpoint ids are allowed"),
            );
        }
        let mut ids = BTreeSet::new();
        for id in &self.remote_allow_ids {
            if id.is_empty()
                || id.len() > MAX_REMOTE_ALLOW_ID_BYTES
                || id.chars().any(char::is_whitespace)
            {
                return invalid(
                    "remote-allow-ids",
                    "ids must be non-empty, bounded, and contain no whitespace",
                );
            }
            if !ids.insert(id) {
                return invalid("remote-allow-ids", format!("duplicate endpoint id `{id}`"));
            }
        }
        if self.hooks.len() > MAX_HOOKS {
            return invalid("hooks", format!("at most {MAX_HOOKS} hooks are allowed"));
        }
        let mut hook_names = BTreeSet::new();
        for hook in &self.hooks {
            if hook.name.is_empty() || hook.name.len() > 64 || !is_safe_name(&hook.name) {
                return invalid(
                    "hooks.name",
                    "must use 1-64 ASCII letters, digits, `.`, `_`, or `-`",
                );
            }
            if !hook_names.insert(&hook.name) {
                return invalid("hooks.name", format!("duplicate hook name `{}`", hook.name));
            }
            hook.command.validate("hooks.command")?;
        }
        Ok(())
    }
}

/// Resolves `$XDG_CONFIG_HOME/fux/config.toml`, falling back to
/// `$HOME/.config/fux/config.toml` when XDG_CONFIG_HOME is unset or empty.
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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields, untagged)]
pub enum Binding {
    Builtin { builtin: BuiltinAction },
    External { external: Command },
}

pub use crate::commands::BuiltinAction;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ClipboardPolicy {
    #[default]
    Disabled,
    ReadOnly,
    WriteOnly,
    ReadWrite,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields, default)]
pub struct NotificationPolicy {
    pub enabled: bool,
    pub notify_blocked: bool,
    pub notify_idle: bool,
    pub remote_clients: bool,
}

impl Default for NotificationPolicy {
    fn default() -> Self {
        Self {
            enabled: true,
            notify_blocked: true,
            notify_idle: true,
            remote_clients: true,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields, default)]
pub struct HistoryLimits {
    pub scrollback_lines: u32,
    pub capture_bytes: usize,
}

impl Default for HistoryLimits {
    fn default() -> Self {
        Self {
            scrollback_lines: 10_000,
            capture_bytes: MAX_CAPTURE_BYTES,
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
        if self.capture_bytes == 0 || self.capture_bytes > MAX_CAPTURE_BYTES {
            return invalid(
                "history.capture-bytes",
                format!("must be 1-{MAX_CAPTURE_BYTES}"),
            );
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields, default)]
pub struct ResourceLimits {
    pub max_units: usize,
    pub max_panes: usize,
    pub max_tabs: usize,
    pub max_popups: usize,
    pub max_status_segments: usize,
    pub max_total_cells: usize,
}

impl Default for ResourceLimits {
    fn default() -> Self {
        Self {
            max_units: 64 * 1024 * 1024,
            max_panes: 128,
            max_tabs: 32,
            max_popups: 16,
            max_status_segments: 32,
            max_total_cells: MAX_TOTAL_CELLS,
        }
    }
}

impl ResourceLimits {
    fn validate(&self) -> Result<(), ConfigError> {
        validate_limit("resources.max-units", self.max_units, MAX_RESOURCE_UNITS)?;
        validate_limit("resources.max-panes", self.max_panes, MAX_PANES)?;
        validate_limit("resources.max-tabs", self.max_tabs, MAX_TABS)?;
        validate_limit("resources.max-popups", self.max_popups, MAX_POPUPS)?;
        validate_limit(
            "resources.max-status-segments",
            self.max_status_segments,
            MAX_STATUS_SEGMENTS,
        )?;
        validate_limit(
            "resources.max-total-cells",
            self.max_total_cells,
            MAX_TOTAL_CELLS,
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct Hook {
    pub name: String,
    pub command: Command,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct ConfigPatch {
    prefix: Option<String>,
    bindings: Option<BTreeMap<String, Binding>>,
    default_command: Option<Command>,
    zor_path: Option<PathBuf>,
    clipboard: Option<ClipboardPolicy>,
    notifications: Option<NotificationPolicy>,
    history: Option<HistoryLimits>,
    resources: Option<ResourceLimits>,
    remote_allow_ids: Option<Vec<String>>,
    local_network: Option<bool>,
    hooks: Option<Vec<Hook>>,
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
        if let Some(value) = self.zor_path {
            config.zor_path = value;
        }
        if let Some(value) = self.clipboard {
            config.clipboard = value;
        }
        if let Some(value) = self.notifications {
            config.notifications = value;
        }
        if let Some(value) = self.history {
            config.history = value;
        }
        if let Some(value) = self.resources {
            config.resources = value;
        }
        if let Some(value) = self.remote_allow_ids {
            config.remote_allow_ids = value;
        }
        if let Some(value) = self.local_network {
            config.local_network = value;
        }
        if let Some(value) = self.hooks {
            config.hooks = value;
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

fn default_bindings() -> BTreeMap<String, Binding> {
    crate::commands::DEFAULT_BINDINGS
        .iter()
        .map(|spec| {
            (
                char::from(spec.key).to_string(),
                Binding::Builtin {
                    builtin: spec.action,
                },
            )
        })
        .collect()
}

fn validate_key_notation(field: &'static str, value: &str) -> Result<u8, ConfigError> {
    if let Some(byte) = crate::commands::key_byte(value) {
        Ok(byte)
    } else {
        invalid(
            field,
            "must encode exactly one byte as a literal byte or `C-x`",
        )
    }
}

fn validate_path(field: &'static str, path: &Path) -> Result<(), ConfigError> {
    if path.as_os_str().is_empty() || path == Path::new(".") || contains_nul(path.as_os_str()) {
        return invalid(field, "must name an executable path without NUL");
    }
    Ok(())
}

#[cfg(unix)]
fn contains_nul(value: &OsStr) -> bool {
    use std::os::unix::ffi::OsStrExt;
    value.as_bytes().contains(&0)
}

#[cfg(not(unix))]
fn contains_nul(value: &OsStr) -> bool {
    value.to_string_lossy().contains('\0')
}

fn validate_limit(field: &'static str, value: usize, maximum: usize) -> Result<(), ConfigError> {
    if value == 0 || value > maximum {
        return invalid(field, format!("must be 1-{maximum}"));
    }
    Ok(())
}

fn is_safe_name(value: &str) -> bool {
    value
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

fn invalid<T>(field: &'static str, reason: impl Into<String>) -> Result<T, ConfigError> {
    Err(ConfigError::Invalid {
        field,
        reason: reason.into(),
    })
}
