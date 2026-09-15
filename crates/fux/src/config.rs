//! `fux.toml`, in the old vocabulary: `prefix`, `default-command`, `clipboard`, `[bindings]`,
//! `[history]`, `[limits]`, `[final]`, `[style]`. Every key is optional over the defaults;
//! unknown keys are errors; the result is the `Limits` resource plus the default command, the
//! prefix, the clipboard policy, the [`Theme`] and the [`Keybindings`] the viewer consumes.

use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};

use bevy_ecs::resource::Resource;
use serde::{Deserialize, Serialize};

use crate::assets::{Keybindings, Theme, ThemeToken, action_name};
use crate::model::{Limits, MAX_LEAVES, MAX_NODES_PER_WORKSPACE};
use crate::viewer::keys::KeyChord;
use crate::wire::Color as WireColor;

pub const MAX_CONFIG_BYTES: u64 = 1024 * 1024;
pub const MAX_SCROLLBACK_LINES: usize = 100_000;
/// Four hours.
pub const MAX_FINAL_RETAIN_MS: u64 = 4 * 60 * 60 * 1000;
pub const MAX_WORKSPACES: usize = 64;
pub const MAX_VIEWERS: usize = 64;
pub const MAX_COMMAND_ARGS: usize = 128;
pub const MAX_COMMAND_ARG_BYTES: usize = 4096;

/// The server holds the loaded configuration as a resource, replaced on reload.
#[derive(Resource, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields, default)]
pub struct Config {
    /// Prefix key in the old notation (`C-b`); interpreted by the viewer.
    pub prefix: String,
    pub default_command: Command,
    /// Whether OSC 52 writes from panes reach the viewer's terminal clipboard.
    pub clipboard: ClipboardPolicy,
    /// Chord (`C-x`, `M-x`, `Esc`, `Space`, a printable key) → action name, merged over the
    /// defaults.
    pub bindings: BTreeMap<String, String>,
    pub history: History,
    pub limits: LimitsSection,
    #[serde(rename = "final")]
    pub final_records: FinalRecords,
    pub style: StyleSection,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            prefix: "C-b".into(),
            default_command: Command::default(),
            clipboard: ClipboardPolicy::default(),
            bindings: BTreeMap::new(),
            history: History::default(),
            limits: LimitsSection::default(),
            final_records: FinalRecords::default(),
            style: StyleSection::default(),
        }
    }
}

/// What a pane's OSC 52 write may do to the enclosing terminal's clipboard. Reading is never
/// offered: the only policies are "off" and "write, bounded, once per write". The viewer holds
/// the loaded policy as a resource.
#[derive(Resource, Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ClipboardPolicy {
    /// Never write to the enclosing terminal's clipboard (`off`, or the old `disabled`).
    #[default]
    #[serde(alias = "disabled")]
    Off,
    /// Application OSC 52 writes reach the terminal clipboard (bounded, once per write).
    WriteOnly,
}

/// A `[style]` value: one of the sixteen ANSI names, `default` (the terminal's own colour) or
/// `none` (keep the cell's colour; for a painted cell the same as `default`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
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
    Default,
    None,
}

impl StyleColor {
    pub fn color(self) -> WireColor {
        match self {
            Self::Black => WireColor::Indexed(0),
            Self::Red => WireColor::Indexed(1),
            Self::Green => WireColor::Indexed(2),
            Self::Yellow => WireColor::Indexed(3),
            Self::Blue => WireColor::Indexed(4),
            Self::Magenta => WireColor::Indexed(5),
            Self::Cyan => WireColor::Indexed(6),
            Self::White => WireColor::Indexed(7),
            Self::BrightBlack => WireColor::Indexed(8),
            Self::BrightRed => WireColor::Indexed(9),
            Self::BrightGreen => WireColor::Indexed(10),
            Self::BrightYellow => WireColor::Indexed(11),
            Self::BrightBlue => WireColor::Indexed(12),
            Self::BrightMagenta => WireColor::Indexed(13),
            Self::BrightCyan => WireColor::Indexed(14),
            Self::BrightWhite => WireColor::Indexed(15),
            Self::Default | Self::None => WireColor::Default,
        }
    }
}

/// `[style]`: the theme tokens by their old names plus the pane-border and focus-ring tokens.
/// `separator` is `pane-border` and `separator-focused`/`pane-border-focused` are `focus-ring`
/// (the focused pane's border is the focus ring); the canonical name wins when both are set.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields, default)]
pub struct StyleSection {
    pub bar: Option<StyleColor>,
    pub bar_background: Option<StyleColor>,
    pub tab: Option<StyleColor>,
    pub tab_active: Option<StyleColor>,
    pub notice: Option<StyleColor>,
    pub separator: Option<StyleColor>,
    pub separator_focused: Option<StyleColor>,
    pub pane_border: Option<StyleColor>,
    pub pane_border_focused: Option<StyleColor>,
    pub focus_ring: Option<StyleColor>,
}

impl StyleSection {
    /// Every set token in precedence order: aliases first, canonical names last.
    fn entries(&self) -> [(ThemeToken, Option<StyleColor>); 10] {
        [
            (ThemeToken::PANE_BORDER, self.separator),
            (ThemeToken::FOCUS_RING, self.separator_focused),
            (ThemeToken::FOCUS_RING, self.pane_border_focused),
            (ThemeToken::BAR, self.bar),
            (ThemeToken::BAR_BACKGROUND, self.bar_background),
            (ThemeToken::TAB, self.tab),
            (ThemeToken::TAB_ACTIVE, self.tab_active),
            (ThemeToken::NOTICE, self.notice),
            (ThemeToken::PANE_BORDER, self.pane_border),
            (ThemeToken::FOCUS_RING, self.focus_ring),
        ]
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

    /// Parses a file's bytes: bounded, UTF-8, then [`Self::from_toml`].
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, ConfigError> {
        if bytes.len() as u64 > MAX_CONFIG_BYTES {
            return invalid("file", format!("may use at most {MAX_CONFIG_BYTES} bytes"));
        }
        let input = core::str::from_utf8(bytes).map_err(|_| ConfigError::Invalid {
            field: "file",
            reason: "must be UTF-8".into(),
        })?;
        Self::from_toml(input)
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
                Self::from_bytes(&bytes)
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(io(error)),
        }
    }

    pub fn validate(&self) -> Result<(), ConfigError> {
        self.keybindings()?;
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

    /// `[style]` over the default theme.
    pub fn theme(&self) -> Theme {
        let mut theme = Theme::default();
        for (token, color) in self.style.entries() {
            if let Some(color) = color {
                theme.colors.insert(token, color.color());
            }
        }
        theme
    }

    /// The prefix and `[bindings]` merged over the default table; refuses a chord that does not
    /// parse or an action nobody registers.
    pub fn keybindings(&self) -> Result<Keybindings, ConfigError> {
        let Some(prefix) = KeyChord::parse(&self.prefix) else {
            return invalid("prefix", format!("{:?} is not a key", self.prefix));
        };
        let mut bindings = Keybindings::with_prefix(prefix);
        for (chord, action) in &self.bindings {
            let Some(chord) = KeyChord::parse(chord) else {
                return invalid("bindings", format!("{chord:?} is not a key"));
            };
            let Some(action) = action_name(action) else {
                return invalid("bindings", format!("{action:?} is not an action"));
            };
            bindings.bindings.insert(chord, action);
        }
        Ok(bindings)
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
        assert!(Config::from_toml("clipboard = 'read-write'").is_err());
    }

    #[test]
    fn clipboard_policy_is_off_unless_write_only() {
        assert_eq!(Config::default().clipboard, ClipboardPolicy::Off);
        assert_eq!(
            Config::from_toml("clipboard = 'write-only'")
                .unwrap()
                .clipboard,
            ClipboardPolicy::WriteOnly
        );
        assert_eq!(
            Config::from_toml("clipboard = 'off'").unwrap().clipboard,
            ClipboardPolicy::Off
        );
        assert_eq!(
            Config::from_toml("clipboard = 'disabled'")
                .unwrap()
                .clipboard,
            ClipboardPolicy::Off,
            "the old spelling still means off"
        );
    }

    #[test]
    fn style_names_and_aliases_load_into_the_theme() {
        let config = Config::from_toml(
            "[style]\nbar = 'white'\nbar-background = 'bright-black'\ntab-active = 'default'\n\
             notice = 'none'\nseparator = 'blue'\nseparator-focused = 'red'\n\
             pane-border-focused = 'bright-cyan'\n",
        )
        .unwrap();
        let theme = config.theme();
        assert_eq!(theme.color(ThemeToken::BAR), WireColor::Indexed(7));
        assert_eq!(
            theme.color(ThemeToken::BAR_BACKGROUND),
            WireColor::Indexed(8)
        );
        assert_eq!(theme.color(ThemeToken::TAB_ACTIVE), WireColor::Default);
        assert_eq!(theme.color(ThemeToken::NOTICE), WireColor::Default);
        assert_eq!(theme.color(ThemeToken::PANE_BORDER), WireColor::Indexed(4));
        assert_eq!(
            theme.color(ThemeToken::FOCUS_RING),
            WireColor::Indexed(14),
            "the canonical name wins over the old alias"
        );
        assert_eq!(
            theme.color(ThemeToken::TAB),
            ThemeToken::TAB.default_color(),
            "unset tokens keep today's colour"
        );
        assert!(Config::from_toml("[style]\nbogus = 'red'").is_err());
        assert!(Config::from_toml("[style]\nbar = 'purple'").is_err());
        assert!(Config::from_toml("[style]\nbar = 'Red'").is_err());
    }

    #[test]
    fn bindings_parse_and_merge_over_the_defaults() {
        let config = Config::from_toml(
            "prefix = 'C-a'\n[bindings]\n'|' = 'split-side'\n'C-x' = 'close-pane'\n\
             'M-x' = 'detach'\nEsc = 'copy-mode'\nSpace = 'zoom'\n'%' = 'help'\n",
        )
        .unwrap();
        let bindings = config.keybindings().unwrap();
        assert_eq!(bindings.prefix, KeyChord::ctrl('a'));
        let action = |chord: &str| {
            bindings
                .bindings
                .get(&KeyChord::parse(chord).unwrap())
                .copied()
        };
        assert_eq!(action("|"), Some("split-side"));
        assert_eq!(action("C-x"), Some("close-pane"));
        assert_eq!(action("M-x"), Some("detach"));
        assert_eq!(action("Esc"), Some("copy-mode"));
        assert_eq!(action("Space"), Some("zoom"));
        assert_eq!(
            action("%"),
            Some("help"),
            "a user chord replaces the default"
        );
        assert_eq!(action("\""), Some("split-below"), "defaults stay");
        assert_eq!(action("S-Tab"), Some("prev-pane"));
        assert_eq!(
            action("C-a"),
            Some("send-prefix"),
            "the prefix sends itself"
        );
        assert!(Config::from_toml("[bindings]\nx = 'explode'").is_err());
        assert!(Config::from_toml("[bindings]\n'C-' = 'zoom'").is_err());
        assert!(Config::from_toml("[bindings]\n'Hyper' = 'zoom'").is_err());
        assert!(Config::from_toml("prefix = ''").is_err());
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
