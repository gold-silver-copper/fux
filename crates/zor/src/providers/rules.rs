//! Passive observation rules (OBSERVATION-CONTRACT.md:150-177): TOML bundles as `bevy_asset`
//! assets. Built-in bundles are embedded; external ones load from `<config_dir>/rules/*.toml`
//! (sorted filename order, a later bundle with the same agent id replaces the earlier one) and
//! hot-reload through the asset server's file watcher. A bundle that fails validation is never
//! published: the previous asset value stays and the problem is reported (`zor/rules.list`).
//! Unmatched screens evaluate to `Unknown`, never implicit idle.

use std::path::{Path, PathBuf};

use bevy_asset::io::Reader;
use bevy_asset::{Asset, AssetLoader, AssetPath, AssetServer, Assets, Handle, LoadContext};
use bevy_ecs::prelude::*;
use bevy_log::warn;
use bevy_reflect::TypePath;
use regex::Regex;
use serde::Deserialize;

use crate::model::AgentState;
use fux::remote::methods::Capture;

/// External bundle directory under the asset root (`<config_dir>`).
pub const RULES_DIR: &str = "rules";
pub const MAX_RULES: usize = 128;
pub const MAX_NAMES: usize = 64;
pub const MAX_NAME_BYTES: usize = 256;
pub const MAX_RULE_ID_BYTES: usize = 128;
pub const MAX_MATCHER_BYTES: usize = 512;
pub const MAX_GATE_DEPTH: usize = 8;
pub const MAX_GATE_DIRECT: usize = 32;
pub const MAX_GATES: usize = 512;
pub const MAX_MATCHERS: usize = 1024;
/// Bound on one external bundle file.
pub const MAX_BUNDLE_BYTES: usize = 256 * 1024;
const MAX_TITLE_CHARS: usize = 256;

const BUILTIN: &[(&str, &str)] = &[
    ("codex.toml", include_str!("../../assets/rules/codex.toml")),
    (
        "claude.toml",
        include_str!("../../assets/rules/claude.toml"),
    ),
    (
        "opencode.toml",
        include_str!("../../assets/rules/opencode.toml"),
    ),
];

// ---------------------------------------------------------------------------------------------
// Schema
// ---------------------------------------------------------------------------------------------

/// The part of the screen a rule reads (OBSERVATION-CONTRACT.md regions; `progress` dropped:
/// fux captures carry none).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Region {
    #[default]
    Whole,
    Bottom(usize),
    Top(usize),
    BottomNonEmpty(usize),
    TopNonEmpty(usize),
    PromptBox,
    AbovePromptBox,
    LastLineAbovePromptBox,
    AfterLastRule,
    AfterLastPromptMarker,
    WholeUnlessAtPrompt,
    Title,
}

impl core::str::FromStr for Region {
    type Err = String;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let simple = match value {
            "whole" => Some(Self::Whole),
            "prompt_box" => Some(Self::PromptBox),
            "above_prompt_box" => Some(Self::AbovePromptBox),
            "last_line_above_prompt_box" => Some(Self::LastLineAbovePromptBox),
            "after_last_rule" => Some(Self::AfterLastRule),
            "after_last_prompt_marker" => Some(Self::AfterLastPromptMarker),
            "whole_unless_at_prompt" => Some(Self::WholeUnlessAtPrompt),
            "title" => Some(Self::Title),
            _ => None,
        };
        if let Some(region) = simple {
            return Ok(region);
        }
        for (prefix, make) in [
            ("bottom(", Self::Bottom as fn(usize) -> Self),
            ("top(", Self::Top),
            ("bottom_non_empty(", Self::BottomNonEmpty),
            ("top_non_empty(", Self::TopNonEmpty),
        ] {
            if let Some(n) = value
                .strip_prefix(prefix)
                .and_then(|tail| tail.strip_suffix(')'))
                .and_then(|n| n.parse().ok())
            {
                return Ok(make(n));
            }
        }
        Err(format!("unknown region {value}"))
    }
}

impl<'de> Deserialize<'de> for Region {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer)?
            .parse()
            .map_err(serde::de::Error::custom)
    }
}

/// A rule's verdict as written in the bundle; `skip` names a screen the rule set recognises
/// as carrying no state (distinct from missing evidence, but still `Unknown` for consumers).
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum RuleState {
    Unknown,
    Working,
    Blocked,
    Idle,
    Skip,
}

impl RuleState {
    pub fn agent_state(self) -> AgentState {
        match self {
            Self::Working => AgentState::Working,
            Self::Blocked => AgentState::Blocked,
            Self::Idle => AgentState::Idle,
            Self::Unknown | Self::Skip => AgentState::Unknown,
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawGate {
    #[serde(default)]
    contains: Vec<String>,
    #[serde(default)]
    regex: Vec<String>,
    #[serde(default)]
    line_regex: Vec<String>,
    #[serde(default)]
    all: Vec<RawGate>,
    #[serde(default)]
    any: Vec<RawGate>,
    #[serde(default)]
    not: Vec<RawGate>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawRule {
    id: String,
    state: RuleState,
    #[serde(default)]
    priority: i32,
    #[serde(default)]
    region: Region,
    #[serde(default)]
    visible_idle: bool,
    #[serde(default)]
    visible_blocker: bool,
    #[serde(default)]
    visible_working: bool,
    #[serde(flatten)]
    gate: RawGate,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawBundle {
    id: String,
    #[serde(default)]
    aliases: Vec<String>,
    #[serde(default)]
    process_names: Vec<String>,
    prompt_marker: Option<String>,
    #[serde(default)]
    block_markers: Vec<String>,
    rules: Vec<RawRule>,
}

/// A compiled matcher: every listed predicate must hold (`any`: at least one child).
#[derive(Clone, Debug, Default)]
pub struct Gate {
    contains: Vec<String>,
    regex: Vec<Regex>,
    line_regex: Vec<Regex>,
    all: Vec<Gate>,
    any: Vec<Gate>,
    not: Vec<Gate>,
}

impl Gate {
    fn matches(&self, text: &str, lower: &str) -> bool {
        self.contains.iter().all(|needle| lower.contains(needle))
            && self.regex.iter().all(|r| r.is_match(text))
            && self
                .line_regex
                .iter()
                .all(|r| text.lines().any(|line| r.is_match(line)))
            && self.all.iter().all(|g| g.matches(text, lower))
            && (self.any.is_empty() || self.any.iter().any(|g| g.matches(text, lower)))
            && self.not.iter().all(|g| !g.matches(text, lower))
    }
}

#[derive(Clone, Debug)]
pub struct Rule {
    pub id: String,
    pub state: RuleState,
    pub priority: i32,
    pub region: Region,
    pub visible_idle: bool,
    pub visible_blocker: bool,
    pub visible_working: bool,
    gate: Gate,
}

/// One validated bundle: the rules of one agent id.
#[derive(Asset, TypePath, Clone, Debug)]
pub struct RulesBundle {
    pub id: String,
    pub aliases: Vec<String>,
    pub process_names: Vec<String>,
    pub prompt_marker: Option<String>,
    pub block_markers: Vec<String>,
    pub rules: Vec<Rule>,
}

#[derive(Debug)]
pub struct RulesError(pub String);

impl core::fmt::Display for RulesError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(&self.0)
    }
}

impl core::error::Error for RulesError {}

impl From<std::io::Error> for RulesError {
    fn from(error: std::io::Error) -> Self {
        Self(error.to_string())
    }
}

fn valid_agent_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

impl RulesBundle {
    /// Parses and validates one bundle (`name` labels errors).
    pub fn parse(name: &str, source: &str) -> Result<Self, RulesError> {
        let fail = |problem: String| RulesError(format!("{name}: {problem}"));
        if source.len() > MAX_BUNDLE_BYTES {
            return Err(fail(format!("bundle exceeds {MAX_BUNDLE_BYTES} bytes")));
        }
        let raw: RawBundle = toml::from_str(source).map_err(|e| fail(e.to_string()))?;
        if !valid_agent_id(&raw.id) {
            return Err(fail(format!("invalid agent id {:?}", raw.id)));
        }
        let bounded_name = |n: &String| {
            !n.is_empty() && n.len() <= MAX_NAME_BYTES && !n.chars().any(char::is_control)
        };
        if raw.aliases.len() > MAX_NAMES
            || raw.process_names.len() > MAX_NAMES
            || !raw
                .aliases
                .iter()
                .chain(&raw.process_names)
                .all(bounded_name)
        {
            return Err(fail(format!(
                "process names/aliases must have at most {MAX_NAMES} bounded names"
            )));
        }
        let mut process_names = raw.process_names;
        if process_names.is_empty() {
            process_names = core::iter::once(raw.id.clone())
                .chain(raw.aliases.iter().cloned())
                .collect();
        }
        if process_names.len() > MAX_NAMES {
            return Err(fail(format!(
                "inherited process names exceed {MAX_NAMES} entries"
            )));
        }
        if raw.rules.len() > MAX_RULES {
            return Err(fail(format!("more than {MAX_RULES} rules")));
        }
        let mut ids = std::collections::HashSet::new();
        let mut totals = (0usize, 0usize);
        let mut rules = Vec::with_capacity(raw.rules.len());
        for rule in raw.rules {
            let rule_fail =
                |problem: &str| RulesError(format!("{name}: rule {}: {problem}", rule.id));
            if rule.id.is_empty()
                || rule.id.len() > MAX_RULE_ID_BYTES
                || rule.id.chars().any(char::is_control)
            {
                return Err(rule_fail("rule id must be 1..128 bytes without controls"));
            }
            if !ids.insert(rule.id.clone()) {
                return Err(rule_fail("duplicate rule id"));
            }
            let mismatched = match rule.state {
                RuleState::Idle => rule.visible_blocker || rule.visible_working,
                RuleState::Blocked => rule.visible_idle || rule.visible_working,
                RuleState::Working => rule.visible_idle || rule.visible_blocker,
                RuleState::Skip => {
                    rule.visible_idle || rule.visible_blocker || rule.visible_working
                }
                RuleState::Unknown => false,
            };
            if mismatched {
                return Err(rule_fail("visible flags disagree with the rule state"));
            }
            let gate = compile_gate(&rule.gate, 0, &mut totals).map_err(|e| rule_fail(&e))?;
            rules.push(Rule {
                id: rule.id,
                state: rule.state,
                priority: rule.priority,
                region: rule.region,
                visible_idle: rule.visible_idle,
                visible_blocker: rule.visible_blocker,
                visible_working: rule.visible_working,
                gate,
            });
        }
        if totals.0 > MAX_GATES || totals.1 > MAX_MATCHERS {
            return Err(fail("rule complexity limit exceeded".into()));
        }
        Ok(Self {
            id: raw.id,
            aliases: raw.aliases,
            process_names,
            prompt_marker: raw.prompt_marker,
            block_markers: raw.block_markers,
            rules,
        })
    }

    /// The best-matching rule of this bundle: highest priority, then earliest in the file.
    pub fn evaluate(&self, screen: &Screen) -> Verdict {
        let mut regions: Vec<(Region, String, String)> = Vec::new();
        let mut winner: Option<(i32, usize)> = None;
        for (index, rule) in self.rules.iter().enumerate() {
            let position = match regions.iter().position(|(r, _, _)| *r == rule.region) {
                Some(p) => p,
                None => {
                    let text = region_text(rule.region, self, screen);
                    let lower = text.to_lowercase();
                    regions.push((rule.region, text, lower));
                    regions.len() - 1
                }
            };
            let Some((_, text, lower)) = regions.get(position) else {
                continue;
            };
            if rule.gate.matches(text, lower)
                && winner.is_none_or(|(priority, _)| rule.priority > priority)
            {
                winner = Some((rule.priority, index));
            }
        }
        match winner.and_then(|(_, index)| self.rules.get(index)) {
            Some(rule) => Verdict {
                state: rule.state.agent_state(),
                rule: Some(rule.id.clone()),
                bundle: Some(self.id.clone()),
            },
            None => Verdict::default(),
        }
    }
}

fn compile_gate(raw: &RawGate, depth: usize, totals: &mut (usize, usize)) -> Result<Gate, String> {
    totals.0 += 1;
    let direct = raw.contains.len()
        + raw.regex.len()
        + raw.line_regex.len()
        + raw.all.len()
        + raw.any.len()
        + raw.not.len();
    totals.1 += raw.contains.len() + raw.regex.len() + raw.line_regex.len();
    if depth > MAX_GATE_DEPTH || direct > MAX_GATE_DIRECT {
        return Err("gate complexity limit exceeded".into());
    }
    if direct == 0 {
        return Err("gate has no matcher".into());
    }
    let mut gate = Gate::default();
    for value in &raw.contains {
        if value.len() > MAX_MATCHER_BYTES {
            return Err(format!("matcher exceeds {MAX_MATCHER_BYTES} bytes"));
        }
        gate.contains.push(value.to_lowercase());
    }
    let compile = |value: &String| -> Result<Regex, String> {
        if value.len() > MAX_MATCHER_BYTES {
            return Err(format!("matcher exceeds {MAX_MATCHER_BYTES} bytes"));
        }
        Regex::new(value).map_err(|e| format!("invalid regex: {e}"))
    };
    for value in &raw.regex {
        gate.regex.push(compile(value)?);
    }
    for value in &raw.line_regex {
        gate.line_regex.push(compile(value)?);
    }
    for child in &raw.all {
        gate.all.push(compile_gate(child, depth + 1, totals)?);
    }
    for child in &raw.any {
        gate.any.push(compile_gate(child, depth + 1, totals)?);
    }
    for child in &raw.not {
        gate.not.push(compile_gate(child, depth + 1, totals)?);
    }
    Ok(gate)
}

// ---------------------------------------------------------------------------------------------
// Screens
// ---------------------------------------------------------------------------------------------

/// Normalized rows of one capture: trailing whitespace and trailing empty rows removed, the
/// text joined with a final newline (the shape every bundle was recorded against).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Screen {
    pub rows: u16,
    pub cols: u16,
    pub lines: Vec<String>,
    pub text: String,
    pub title: String,
}

impl Screen {
    pub fn from_lines(rows: u16, cols: u16, mut lines: Vec<String>, title: &str) -> Self {
        for line in &mut lines {
            line.truncate(line.trim_end().len());
        }
        while lines.last().is_some_and(String::is_empty) {
            lines.pop();
        }
        let mut text = lines.join("\n");
        if !text.is_empty() {
            text.push('\n');
        }
        Self {
            rows,
            cols,
            lines,
            text,
            title: title.chars().take(MAX_TITLE_CHARS).collect(),
        }
    }

    /// The screen rows of a `fux/pane.capture` reply (the last `rows` lines: any scrollback
    /// the caller asked for precedes them).
    pub fn from_capture(capture: Capture) -> Self {
        let rows = usize::from(capture.rows);
        let mut lines = capture.lines;
        if lines.len() > rows {
            lines.drain(..lines.len() - rows);
        }
        Self::from_lines(capture.rows, capture.cols, lines, &capture.title)
    }
}

/// The classification of one screen.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Verdict {
    /// `Unknown` when no rule matched.
    pub state: AgentState,
    pub rule: Option<String>,
    pub bundle: Option<String>,
}

fn join(lines: &[String]) -> String {
    if lines.is_empty() {
        String::new()
    } else {
        format!("{}\n", lines.join("\n"))
    }
}

fn marker_index(lines: &[String], marker: Option<&str>) -> Option<usize> {
    let marker = marker?;
    lines.iter().rposition(|line| {
        line == marker
            || line
                .strip_prefix(marker)
                .is_some_and(|tail| tail.starts_with(' '))
    })
}

fn horizontal_rule(line: &str) -> bool {
    let value = line.trim();
    let count = value.chars().take_while(|ch| *ch == '─').count();
    count >= 3 || (count == 2 && value.chars().count() == 2)
}

fn rule_rows(lines: &[String]) -> Vec<usize> {
    lines
        .iter()
        .enumerate()
        .filter(|(_, line)| horizontal_rule(line))
        .map(|(index, _)| index)
        .collect()
}

fn region_text(region: Region, bundle: &RulesBundle, screen: &Screen) -> String {
    let lines = &screen.lines;
    match region {
        Region::Whole => screen.text.clone(),
        Region::Title => screen.title.clone(),
        Region::Bottom(n) => join(
            lines
                .get(lines.len().saturating_sub(n)..)
                .unwrap_or_default(),
        ),
        Region::Top(n) => join(lines.get(..n.min(lines.len())).unwrap_or_default()),
        Region::BottomNonEmpty(n) => lines
            .iter()
            .enumerate()
            .rev()
            .filter(|(_, line)| !line.is_empty())
            .nth(n.saturating_sub(1))
            .map_or_else(String::new, |(index, _)| {
                join(lines.get(index..).unwrap_or_default())
            }),
        Region::TopNonEmpty(n) => lines
            .iter()
            .enumerate()
            .filter(|(_, line)| !line.is_empty())
            .nth(n.saturating_sub(1))
            .map_or_else(String::new, |(index, _)| {
                join(lines.get(..=index).unwrap_or_default())
            }),
        Region::PromptBox => {
            let rules = rule_rows(lines);
            match (rules.get(rules.len().wrapping_sub(2)), rules.last()) {
                (Some(lower), Some(upper)) if rules.len() >= 2 => {
                    join(lines.get(lower + 1..*upper).unwrap_or_default())
                }
                _ => String::new(),
            }
        }
        Region::AbovePromptBox | Region::LastLineAbovePromptBox => {
            let rules = rule_rows(lines);
            let end = if rules.len() >= 2 {
                rules.get(rules.len() - 2).copied()
            } else {
                None
            };
            let above = end.map_or(lines.as_slice(), |index| {
                lines.get(..index).unwrap_or_default()
            });
            if region == Region::LastLineAbovePromptBox {
                above
                    .iter()
                    .rev()
                    .find(|line| !line.is_empty())
                    .map_or_else(String::new, |line| join(core::slice::from_ref(line)))
            } else {
                join(above)
            }
        }
        Region::AfterLastRule => lines
            .iter()
            .rposition(|line| horizontal_rule(line))
            .map_or_else(String::new, |index| {
                join(lines.get(index + 1..).unwrap_or_default())
            }),
        Region::AfterLastPromptMarker => marker_index(lines, bundle.prompt_marker.as_deref())
            .map_or_else(String::new, |index| {
                join(lines.get(index + 1..).unwrap_or_default())
            }),
        Region::WholeUnlessAtPrompt => match marker_index(lines, bundle.prompt_marker.as_deref()) {
            None => screen.text.clone(),
            Some(index) => {
                let blocked = lines
                    .get(index + 1..)
                    .unwrap_or_default()
                    .iter()
                    .any(|line| {
                        bundle
                            .block_markers
                            .iter()
                            .any(|marker| line.starts_with(marker))
                    });
                if blocked {
                    screen.text.clone()
                } else {
                    String::new()
                }
            }
        },
    }
}

// ---------------------------------------------------------------------------------------------
// Asset loader
// ---------------------------------------------------------------------------------------------

/// Loads `*.toml` under the rules directory as [`RulesBundle`]s; a bundle that fails
/// validation is a load error, so the previously published asset stays.
#[derive(Default, TypePath)]
pub struct RulesLoader;

impl AssetLoader for RulesLoader {
    type Asset = RulesBundle;
    type Settings = ();
    type Error = RulesError;

    async fn load(
        &self,
        reader: &mut dyn Reader,
        _settings: &Self::Settings,
        context: &mut LoadContext<'_>,
    ) -> Result<Self::Asset, Self::Error> {
        let mut bytes = Vec::new();
        reader.read_to_end(&mut bytes).await?;
        let name = context.path().to_string();
        let source = String::from_utf8(bytes).map_err(|e| RulesError(format!("{name}: {e}")))?;
        RulesBundle::parse(&name, &source)
    }

    fn extensions(&self) -> &[&str] {
        &["toml"]
    }
}

// ---------------------------------------------------------------------------------------------
// The collection
// ---------------------------------------------------------------------------------------------

/// Built-in bundles plus the external handles, in override order.
#[derive(Resource, Debug)]
pub struct Rules {
    /// On-disk asset root (`AssetPlugin::file_path`); `None` keeps the defaults only.
    pub root: Option<PathBuf>,
    builtin: Vec<RulesBundle>,
    /// `(asset path, handle)` in sorted filename order.
    external: Vec<(String, Handle<RulesBundle>)>,
    /// Bumped whenever an external bundle is (re)published.
    pub generation: u64,
    /// The last load failure (`path: reason`); cleared when that path publishes.
    pub problem: Option<String>,
}

/// One row of `zor/rules.list`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BundleInfo {
    pub id: String,
    /// `builtin:<file>` or the external asset path.
    pub source: String,
    pub rules: usize,
    /// Whether this bundle is the effective one for its id.
    pub effective: bool,
}

impl Rules {
    pub fn new(root: Option<PathBuf>) -> Self {
        let builtin = BUILTIN
            .iter()
            .filter_map(|(name, source)| match RulesBundle::parse(name, source) {
                Ok(bundle) => Some(bundle),
                Err(e) => {
                    warn!("built-in rules {name}: {e}");
                    None
                }
            })
            .collect();
        Self {
            root,
            builtin,
            external: Vec::new(),
            generation: 0,
            problem: None,
        }
    }

    /// `<root>/rules/*.toml` as asset paths, sorted by filename.
    pub fn discover(root: &Path) -> Vec<String> {
        let Ok(entries) = std::fs::read_dir(root.join(RULES_DIR)) else {
            return Vec::new();
        };
        let mut files: Vec<String> = entries
            .filter_map(Result::ok)
            .filter(|e| e.path().extension().is_some_and(|x| x == "toml"))
            .filter_map(|e| e.file_name().into_string().ok())
            .map(|file| format!("{RULES_DIR}/{file}"))
            .collect();
        files.sort();
        files
    }

    /// (Re)loads the external set: new paths are loaded, known ones reloaded, vanished ones
    /// dropped. Returns how many paths were requested.
    pub fn reload(&mut self, server: &AssetServer) -> usize {
        let Some(root) = self.root.clone() else {
            return 0;
        };
        let paths = Self::discover(&root);
        let mut next = Vec::with_capacity(paths.len());
        for path in paths {
            let known = self
                .external
                .iter()
                .find(|(p, _)| *p == path)
                .map(|(_, h)| h.clone());
            let handle = match known {
                Some(handle) => {
                    server.reload(AssetPath::from(path.clone()));
                    handle
                }
                None => server.load::<RulesBundle>(AssetPath::from(path.clone())),
            };
            next.push((path, handle));
        }
        self.external = next;
        self.external.len()
    }

    /// Effective bundles by agent id: built-ins first, each external bundle replacing the
    /// earlier one with the same id.
    pub fn effective<'a>(&'a self, assets: &'a Assets<RulesBundle>) -> Vec<&'a RulesBundle> {
        let mut out: Vec<&RulesBundle> = self.builtin.iter().collect();
        for (_, handle) in &self.external {
            let Some(bundle) = assets.get(handle) else {
                continue;
            };
            match out.iter().position(|b| b.id == bundle.id) {
                Some(i) => {
                    if let Some(slot) = out.get_mut(i) {
                        *slot = bundle;
                    }
                }
                None => out.push(bundle),
            }
        }
        out
    }

    pub fn list(&self, assets: &Assets<RulesBundle>) -> Vec<BundleInfo> {
        let effective = self.effective(assets);
        let is_effective =
            |bundle: &RulesBundle| effective.iter().any(|b| core::ptr::eq(*b, bundle));
        let mut rows: Vec<BundleInfo> = self
            .builtin
            .iter()
            .zip(BUILTIN)
            .map(|(bundle, (name, _))| BundleInfo {
                id: bundle.id.clone(),
                source: format!("builtin:{name}"),
                rules: bundle.rules.len(),
                effective: is_effective(bundle),
            })
            .collect();
        for (path, handle) in &self.external {
            if let Some(bundle) = assets.get(handle) {
                rows.push(BundleInfo {
                    id: bundle.id.clone(),
                    source: path.clone(),
                    rules: bundle.rules.len(),
                    effective: is_effective(bundle),
                });
            }
        }
        rows
    }

    /// Classifies a screen: with `bundle` the named agent's rules only; without, every
    /// effective bundle in id order, the first match by priority winning.
    pub fn evaluate(
        &self,
        assets: &Assets<RulesBundle>,
        bundle: Option<&str>,
        screen: &Screen,
    ) -> Verdict {
        let effective = self.effective(assets);
        match bundle {
            Some(id) => effective
                .iter()
                .find(|b| b.id == id)
                .map(|b| b.evaluate(screen))
                .unwrap_or_default(),
            None => {
                let mut best: Option<(i32, Verdict)> = None;
                for b in effective {
                    let verdict = b.evaluate(screen);
                    let Some(rule) = verdict.rule.as_deref() else {
                        continue;
                    };
                    let priority = b
                        .rules
                        .iter()
                        .find(|r| r.id == rule)
                        .map_or(0, |r| r.priority);
                    if best.as_ref().is_none_or(|(p, _)| priority > *p) {
                        best = Some((priority, verdict));
                    }
                }
                best.map(|(_, v)| v).unwrap_or_default()
            }
        }
    }
}

/// `Startup`: request every external bundle. Discovery reads the directory directly; loading
/// goes through the asset server (and its file watcher).
pub(super) fn load_external(world: &mut World) {
    let server = world.resource::<AssetServer>().clone();
    let mut rules = world.resource_mut::<Rules>();
    let n = rules.reload(&server);
    if n > 0 {
        bevy_log::info!("rules: {n} external bundle(s) requested");
    }
}
