//! `zor-plugin.toml`: the contract between zor and a plugin (prompt 4.4). Package metadata,
//! supported platforms, optional build/startup commands and the entrypoints zor can run:
//! actions, event hooks, panes and link handlers. Every table is `deny_unknown_fields`;
//! commands are argv arrays run without a shell; item-level `platforms` override the
//! top-level list and non-matching items are dropped at load ([`Manifest::for_platform`]).

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::model::ids::valid_id;

pub const MANIFEST_FILE: &str = "zor-plugin.toml";
/// Bound on a manifest file.
pub const MAX_MANIFEST_BYTES: u64 = 256 * 1024;
/// Entries per table.
pub const MAX_ENTRIES: usize = 64;

/// Where a plugin entry's process is shown, as a composition of `fux/*` calls
/// (`crate::plugins` "Placements").
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Placement {
    /// A headless process: no pane.
    #[default]
    None,
    /// An absolute node over the whole root of the target pane.
    Overlay,
    /// A new leaf to the right of the target pane.
    Split,
    /// A new root (tab) in the workspace.
    Tab,
    /// `Split`, then zoomed for every viewer of the workspace.
    Zoomed,
    /// An absolute node centred over the root, half its size.
    Popup,
}

impl Placement {
    pub const fn name(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Overlay => "overlay",
            Self::Split => "split",
            Self::Tab => "tab",
            Self::Zoomed => "zoomed",
            Self::Popup => "popup",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PaneKind {
    /// `fux/node.spawn` with a `PaneTemplate` running the command.
    #[default]
    Terminal,
    /// `fux/surface.open` on a spawned node; the command drives it over `fux/surface.update`
    /// with `ZOR_PLUGIN_SURFACE` set.
    Surface,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Action {
    pub id: String,
    pub title: String,
    pub command: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keybinding: Option<String>,
    #[serde(default)]
    pub placement: Placement,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub platforms: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EventHook {
    /// `zor/<Name>` or `fux/<Name>`; `*` matches any run of characters.
    pub pattern: String,
    pub command: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub platforms: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Pane {
    pub id: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub kind: PaneKind,
    #[serde(default = "default_pane_placement")]
    pub placement: Placement,
    pub command: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub platforms: Vec<String>,
}

fn default_pane_placement() -> Placement {
    Placement::Overlay
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Link {
    pub id: String,
    /// A regular expression over the whole URL.
    pub pattern: String,
    pub command: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub platforms: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    /// The plugin id: `1..=64` of `[A-Za-z0-9_-]`.
    pub name: String,
    pub version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// `macos`, `linux`, ...; empty means every platform.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub platforms: Vec<String>,
    /// Run once at install/link, in the plugin root.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub build: Vec<String>,
    /// Run whenever the plugin becomes active (enable, server start).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub startup: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub actions: Vec<Action>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub events: Vec<EventHook>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub panes: Vec<Pane>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub links: Vec<Link>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManifestError {
    Io(String),
    Parse(String),
    /// A semantic rule failed: `(what, why)`.
    Invalid(String, String),
    /// The plugin does not declare the current platform.
    Platform { declared: Vec<String>, current: String },
}

impl core::fmt::Display for ManifestError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "manifest: {e}"),
            Self::Parse(e) => write!(f, "manifest: {e}"),
            Self::Invalid(what, why) => write!(f, "manifest: {what}: {why}"),
            Self::Platform { declared, current } => write!(
                f,
                "manifest: platforms {declared:?} do not include {current}"
            ),
        }
    }
}

impl core::error::Error for ManifestError {}

/// The current platform name as manifests spell it (`std::env::consts::OS`).
pub const fn current_platform() -> &'static str {
    std::env::consts::OS
}

impl Manifest {
    /// Parses and validates; platform filtering is separate ([`Self::for_platform`]).
    pub fn parse(text: &str) -> Result<Self, ManifestError> {
        let manifest: Self = toml::from_str(text).map_err(|e| ManifestError::Parse(e.to_string()))?;
        manifest.validate()?;
        Ok(manifest)
    }

    /// Reads `path` (a `zor-plugin.toml` or a directory containing one).
    pub fn read(path: &Path) -> Result<Self, ManifestError> {
        let file = manifest_file(path);
        let meta = std::fs::metadata(&file)
            .map_err(|e| ManifestError::Io(format!("{}: {e}", file.display())))?;
        if meta.len() > MAX_MANIFEST_BYTES {
            return Err(ManifestError::Io(format!(
                "{}: larger than {MAX_MANIFEST_BYTES} bytes",
                file.display()
            )));
        }
        let text = std::fs::read_to_string(&file)
            .map_err(|e| ManifestError::Io(format!("{}: {e}", file.display())))?;
        Self::parse(&text)
    }

    fn validate(&self) -> Result<(), ManifestError> {
        let invalid = |what: &str, why: &str| Err(ManifestError::Invalid(what.into(), why.into()));
        if !valid_id(&self.name) {
            return invalid("name", "must be 1..=64 of [A-Za-z0-9_-]");
        }
        if self.version.is_empty() || self.version.len() > 64 {
            return invalid("version", "must be 1..=64 characters");
        }
        if self.actions.len() > MAX_ENTRIES
            || self.events.len() > MAX_ENTRIES
            || self.panes.len() > MAX_ENTRIES
            || self.links.len() > MAX_ENTRIES
        {
            return invalid("tables", "at most 64 entries each");
        }
        for platform in self
            .platforms
            .iter()
            .chain(self.actions.iter().flat_map(|a| &a.platforms))
            .chain(self.events.iter().flat_map(|e| &e.platforms))
            .chain(self.panes.iter().flat_map(|p| &p.platforms))
            .chain(self.links.iter().flat_map(|l| &l.platforms))
        {
            if platform.is_empty() || !platform.bytes().all(|b| b.is_ascii_lowercase()) {
                return invalid("platforms", "names are lowercase ASCII (macos, linux)");
            }
        }
        check_command("build", &self.build, true)?;
        check_command("startup", &self.startup, true)?;
        let mut ids = std::collections::HashSet::new();
        for action in &self.actions {
            if !valid_id(&action.id) {
                return invalid("actions", "id must be 1..=64 of [A-Za-z0-9_-]");
            }
            if !ids.insert(&action.id) {
                return Err(ManifestError::Invalid(
                    "actions".into(),
                    format!("duplicate id {}", action.id),
                ));
            }
            if action.title.is_empty() {
                return Err(ManifestError::Invalid(
                    format!("action {}", action.id),
                    "title is required".into(),
                ));
            }
            check_command(&format!("action {}", action.id), &action.command, false)?;
            if let Some(chord) = &action.keybinding
                && (chord.is_empty() || chord.len() > 64)
            {
                return Err(ManifestError::Invalid(
                    format!("action {}", action.id),
                    "keybinding must be 1..=64 characters".into(),
                ));
            }
        }
        for (index, hook) in self.events.iter().enumerate() {
            let what = format!("events[{index}]");
            if hook.pattern.is_empty() || hook.pattern.len() > 128 {
                return Err(ManifestError::Invalid(
                    what,
                    "pattern must be 1..=128 characters".into(),
                ));
            }
            if !(hook.pattern.starts_with("zor/")
                || hook.pattern.starts_with("fux/")
                || hook.pattern.starts_with('*'))
            {
                return Err(ManifestError::Invalid(
                    what,
                    "pattern must start with `zor/`, `fux/` or `*`".into(),
                ));
            }
            check_command(&what, &hook.command, false)?;
        }
        ids.clear();
        for pane in &self.panes {
            if !valid_id(&pane.id) {
                return invalid("panes", "id must be 1..=64 of [A-Za-z0-9_-]");
            }
            if !ids.insert(&pane.id) {
                return Err(ManifestError::Invalid(
                    "panes".into(),
                    format!("duplicate id {}", pane.id),
                ));
            }
            if pane.placement == Placement::None {
                return Err(ManifestError::Invalid(
                    format!("pane {}", pane.id),
                    "placement must name a pane placement".into(),
                ));
            }
            check_command(&format!("pane {}", pane.id), &pane.command, false)?;
        }
        ids.clear();
        for link in &self.links {
            if !valid_id(&link.id) {
                return invalid("links", "id must be 1..=64 of [A-Za-z0-9_-]");
            }
            if !ids.insert(&link.id) {
                return Err(ManifestError::Invalid(
                    "links".into(),
                    format!("duplicate id {}", link.id),
                ));
            }
            if link.pattern.len() > 512 {
                return Err(ManifestError::Invalid(
                    format!("link {}", link.id),
                    "pattern must be at most 512 characters".into(),
                ));
            }
            regex::Regex::new(&link.pattern).map_err(|e| {
                ManifestError::Invalid(format!("link {}", link.id), format!("pattern: {e}"))
            })?;
            check_command(&format!("link {}", link.id), &link.command, false)?;
        }
        Ok(())
    }

    /// Refuses a plugin that excludes `platform`, then drops every item that excludes it.
    pub fn for_platform(mut self, platform: &str) -> Result<Self, ManifestError> {
        if !supports(&self.platforms, platform) {
            return Err(ManifestError::Platform {
                declared: self.platforms,
                current: platform.into(),
            });
        }
        self.actions.retain(|a| supports(&a.platforms, platform));
        self.events.retain(|e| supports(&e.platforms, platform));
        self.panes.retain(|p| supports(&p.platforms, platform));
        self.links.retain(|l| supports(&l.platforms, platform));
        Ok(self)
    }

    pub fn action(&self, id: &str) -> Option<&Action> {
        self.actions.iter().find(|a| a.id == id)
    }

    pub fn pane(&self, id: &str) -> Option<&Pane> {
        self.panes.iter().find(|p| p.id == id)
    }
}

fn supports(platforms: &[String], platform: &str) -> bool {
    platforms.is_empty() || platforms.iter().any(|p| p == platform)
}

fn check_command(what: &str, command: &[String], optional: bool) -> Result<(), ManifestError> {
    if command.is_empty() {
        return if optional {
            Ok(())
        } else {
            Err(ManifestError::Invalid(
                what.into(),
                "command is required (an argv array)".into(),
            ))
        };
    }
    if command.len() > 256 || command.iter().any(|w| w.is_empty() || w.len() > 4096) {
        return Err(ManifestError::Invalid(
            what.into(),
            "command words are 1..=4096 bytes, at most 256 words".into(),
        ));
    }
    Ok(())
}

/// `path` itself when it is a file, else `path/zor-plugin.toml`.
pub fn manifest_file(path: &Path) -> std::path::PathBuf {
    if path.is_file() {
        path.to_path_buf()
    } else {
        path.join(MANIFEST_FILE)
    }
}

/// `*`-glob match of an event name (`zor/TaskClosed`) against a hook pattern.
pub fn pattern_matches(pattern: &str, name: &str) -> bool {
    fn go(p: &[u8], n: &[u8]) -> bool {
        match p.split_first() {
            None => n.is_empty(),
            Some((b'*', rest)) => (0..=n.len()).any(|i| go(rest, n.get(i..).unwrap_or_default())),
            Some((c, rest)) => n.first() == Some(c) && go(rest, n.get(1..).unwrap_or_default()),
        }
    }
    go(pattern.as_bytes(), name.as_bytes())
}
