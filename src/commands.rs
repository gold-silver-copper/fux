//! The one command registry: configured prefix and bindings, labels, grouping, contextual
//! availability, viewer dispatch and CLI binding output all read from here.

use crate::view::Frame;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const DEFAULT_PREFIX: u8 = 1;

/// The one command table: every action with its group, label and default key. The enum, the
/// registry order, the labels, the grouping and the default bindings all come from this list.
macro_rules! actions {
    ($($variant:ident => $group:ident, $label:literal, $key:literal;)+) => {
        #[derive(
            Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize,
        )]
        #[serde(rename_all = "kebab-case")]
        pub enum Action {
            $($variant,)+
        }

        impl Action {
            /// Registry order: the order of the popup and of `fux bindings` within a group.
            pub const ALL: &'static [Self] = &[$(Self::$variant,)+];

            pub const fn group(self) -> Group {
                match self {
                    $(Self::$variant => Group::$group,)+
                }
            }

            pub const fn label(self) -> &'static str {
                match self {
                    $(Self::$variant => $label,)+
                }
            }
        }

        pub const DEFAULT_BINDINGS: &[BindingSpec] = &[
            $(BindingSpec { key: $key, action: Action::$variant },)+
        ];
    };
}

actions! {
    SplitSide => Panes, "split side by side", b'|';
    SplitStack => Panes, "split stacked", b'-';
    FocusLeft => Focus, "focus left", b'h';
    FocusRight => Focus, "focus right", b'l';
    FocusUp => Focus, "focus up", b'k';
    FocusDown => Focus, "focus down", b'j';
    ClosePane => Panes, "close pane", b'x';
    ResizeMode => Panes, "resize split", b'r';
    CopyMode => Panes, "history and copy", b'[';
    NewTab => Tabs, "new tab", b't';
    NextTab => Tabs, "next tab", b'n';
    PreviousTab => Tabs, "previous tab", b'p';
    ChooseTab => Tabs, "choose tab", b'w';
    RenameTab => Tabs, "rename tab", b',';
    CloseTab => Tabs, "close tab", b'c';
    ChooseWorkspace => Workspaces, "choose workspace", b's';
    NewWorkspace => Workspaces, "new workspace", b'a';
    Detach => Session, "detach", b'd';
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub enum Group {
    Panes,
    Focus,
    Tabs,
    Workspaces,
    Session,
}

impl Group {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Panes => "Panes",
            Self::Focus => "Focus",
            Self::Tabs => "Tabs",
            Self::Workspaces => "Workspaces",
            Self::Session => "Session",
        }
    }
}

pub struct BindingSpec {
    pub key: u8,
    pub action: Action,
}

impl Action {
    /// The obvious contextual restrictions shared by the popup and viewer dispatch. The server
    /// remains authoritative for limits and for changes made by other viewers.
    pub fn unavailable(self, frame: &Frame, workspaces: bool) -> Option<&'static str> {
        let visible = frame.layout.len();
        let live_focus = frame.focused_pane().is_some_and(|pane| pane.exit.is_none());
        match self {
            Self::Detach | Self::NewTab => None,
            Self::ChooseWorkspace | Self::NewWorkspace => {
                (!workspaces).then_some("Not available through this attachment")
            }
            Self::ClosePane | Self::CopyMode => (!live_focus).then_some("No live pane"),
            Self::SplitSide | Self::SplitStack => {
                frame.focused.is_none().then_some("No active pane")
            }
            Self::NextTab | Self::PreviousTab => (frame.tabs.len() < 2).then_some("Only one tab"),
            Self::ChooseTab | Self::RenameTab | Self::CloseTab => {
                frame.active_tab.is_none().then_some("No active tab")
            }
            Self::FocusLeft
            | Self::FocusRight
            | Self::FocusUp
            | Self::FocusDown
            | Self::ResizeMode => (visible < 2).then_some("No split to adjust"),
        }
    }
}

/// The key a byte stands for without Shift: letters fold to lowercase and the shifted symbols of
/// a US layout fold to their unshifted twins (`|` → `\`, `_` → `-`, `?` → `/`, …). Bindings are
/// matched on this form, so `X` and `x` are the same key and `\` triggers a binding on `|`.
#[must_use]
pub const fn canonical_key(key: u8) -> u8 {
    match key {
        b'A'..=b'Z' => key.to_ascii_lowercase(),
        b'!' => b'1',
        b'@' => b'2',
        b'#' => b'3',
        b'$' => b'4',
        b'%' => b'5',
        b'^' => b'6',
        b'&' => b'7',
        b'*' => b'8',
        b'(' => b'9',
        b')' => b'0',
        b'_' => b'-',
        b'+' => b'=',
        b'{' => b'[',
        b'}' => b']',
        b'|' => b'\\',
        b':' => b';',
        b'"' => b'\'',
        b'<' => b',',
        b'>' => b'.',
        b'?' => b'/',
        b'~' => b'`',
        _ => key,
    }
}

pub fn key_name(key: u8) -> String {
    match key {
        0 => "C-@".to_owned(),
        1..=26 => format!("C-{}", char::from(key + b'a' - 1)),
        27 => "Esc".to_owned(),
        28..=31 => format!("C-{}", char::from(key + b'@')),
        32 => "Space".to_owned(),
        127 => "DEL".to_owned(),
        128..=255 => format!("0x{key:02x}"),
        _ => char::from(key).to_string(),
    }
}

/// Decode the single-byte notation shared by configuration and input routing.
pub fn key_byte(value: &str) -> Option<u8> {
    if let Some(value) = value.strip_prefix("C-")
        && value.len() == 1
    {
        return value
            .bytes()
            .next()
            .map(|byte| byte.to_ascii_uppercase() & 0x1f);
    }
    match value {
        "Esc" => return Some(27),
        "Space" => return Some(32),
        "DEL" => return Some(127),
        _ => {}
    }
    if let Some(hex) = value.strip_prefix("0x")
        && hex.len() == 2
    {
        return u8::from_str_radix(hex, 16).ok();
    }
    (value.len() == 1)
        .then(|| value.as_bytes().first().copied())
        .flatten()
}

/// Viewer-facing bindings published with every frame. Keys are single bytes, so the map is
/// bounded by construction.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClientBindings {
    prefix: u8,
    /// Actions this build does not know (a server of another release) are dropped rather than
    /// failing the whole frame, so binding-set changes never break attachment.
    #[serde(deserialize_with = "known_actions")]
    bindings: BTreeMap<u8, Action>,
}

fn known_actions<'de, D>(deserializer: D) -> Result<BTreeMap<u8, Action>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Maybe {
        Known(Action),
        Unknown(serde::de::IgnoredAny),
    }
    let raw: BTreeMap<u8, Maybe> = BTreeMap::deserialize(deserializer)?;
    Ok(raw
        .into_iter()
        .filter_map(|(key, action)| match action {
            Maybe::Known(action) => Some((key, action)),
            Maybe::Unknown(_) => None,
        })
        .collect())
}

impl ClientBindings {
    pub fn new(prefix: u8, bindings: impl IntoIterator<Item = (u8, Action)>) -> Self {
        let mut map = BTreeMap::new();
        for (key, action) in bindings {
            if canonical_key(key) != canonical_key(prefix) {
                map.insert(key, action);
            }
        }
        Self {
            prefix,
            bindings: map,
        }
    }

    #[must_use]
    pub fn prefix(&self) -> u8 {
        self.prefix
    }

    /// The action bound to `key`, matched without Shift (see [`canonical_key`]).
    #[must_use]
    pub fn action(&self, key: u8) -> Option<Action> {
        if let Some(action) = self.bindings.get(&key) {
            return Some(*action);
        }
        let wanted = canonical_key(key);
        self.bindings
            .iter()
            .find(|(bound, _)| canonical_key(**bound) == wanted)
            .map(|(_, action)| *action)
    }

    /// Bindings ordered by group, then by the registry's action order (`| - x r [` rather than
    /// byte order), then key.
    pub fn entries(&self) -> Vec<(u8, Action)> {
        let mut entries: Vec<_> = self
            .bindings
            .iter()
            .map(|(key, action)| (*key, *action))
            .collect();
        entries.sort_by_key(|(key, action)| {
            let rank = Action::ALL
                .iter()
                .position(|a| a == action)
                .unwrap_or(usize::MAX);
            (action.group(), rank, *key)
        });
        entries
    }
}

impl Default for ClientBindings {
    fn default() -> Self {
        Self::new(
            DEFAULT_PREFIX,
            DEFAULT_BINDINGS.iter().map(|spec| (spec.key, spec.action)),
        )
    }
}

/// Resolve the configured registry once for execution, the popup and `fux bindings`. The
/// configuration's own validation rejects unknown notation, prefix clashes and Shift twins.
pub fn configured_bindings(config: &crate::config::Config) -> anyhow::Result<ClientBindings> {
    config.validate()?;
    let byte =
        |key: &str| key_byte(key).ok_or_else(|| anyhow::anyhow!("`{key}` must encode one byte"));
    let prefix = byte(&config.prefix)?;
    let bindings = config
        .bindings
        .iter()
        .map(|(key, action)| Ok((byte(key)?, *action)))
        .collect::<anyhow::Result<Vec<_>>>()?;
    Ok(ClientBindings::new(prefix, bindings))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_action_is_bound_once_by_default_and_labelled() {
        let bindings = ClientBindings::default();
        for action in Action::ALL {
            assert!(
                bindings.entries().iter().any(|(_, bound)| bound == action),
                "{action:?} unbound"
            );
            assert!(!action.label().is_empty());
        }
        assert_eq!(bindings.entries().len(), Action::ALL.len());
        assert_eq!(bindings.action(bindings.prefix()), None);
    }

    #[test]
    fn unknown_actions_from_another_release_are_dropped_not_fatal() {
        let json = r#"{"prefix":1,"bindings":{"63":"help","120":"close-pane","88":"close-tab"}}"#;
        let bindings: ClientBindings = serde_json::from_str(json).unwrap_or_default();
        assert_eq!(bindings.action(b'x'), Some(Action::ClosePane));
        assert_eq!(
            bindings.action(b'X'),
            Some(Action::CloseTab),
            "an older server's shifted binding still matches exactly first"
        );
        assert_eq!(bindings.action(b'?'), None, "unknown `help` dropped");
        assert_eq!(bindings.entries().len(), 2);
    }

    #[test]
    fn key_notation_round_trips() {
        assert_eq!(key_byte("C-a"), Some(1));
        assert_eq!(key_byte("C-A"), Some(1));
        assert_eq!(key_byte("Esc"), Some(27));
        assert_eq!(key_byte("|"), Some(b'|'));
        assert_eq!(key_byte("ab"), None);
        assert_eq!(key_byte(""), None);
        for key in [1_u8, 27, 32, 127, b'x', 200] {
            assert_eq!(key_byte(&key_name(key)), Some(key), "{key}");
        }
    }

    #[test]
    fn availability_follows_frame_context() {
        let frame = Frame::default();
        assert!(Action::SplitSide.unavailable(&frame, true).is_some());
        assert!(Action::ResizeMode.unavailable(&frame, true).is_some());
        assert!(Action::NextTab.unavailable(&frame, true).is_some());
        assert!(Action::ChooseWorkspace.unavailable(&frame, false).is_some());
        assert!(Action::ChooseWorkspace.unavailable(&frame, true).is_none());
        assert!(Action::Detach.unavailable(&frame, false).is_none());
    }

    #[test]
    fn keys_match_without_shift_and_shifted_twins_are_rejected() {
        let bindings = ClientBindings::default();
        assert_eq!(
            bindings.action(b'\\'),
            Some(Action::SplitSide),
            "\\ is | without Shift"
        );
        assert_eq!(bindings.action(b'|'), Some(Action::SplitSide));
        assert_eq!(bindings.action(b'_'), Some(Action::SplitStack));
        assert_eq!(bindings.action(b'X'), Some(Action::ClosePane));
        assert_eq!(bindings.action(b'C'), Some(Action::CloseTab));
        assert_eq!(bindings.action(b'A'), Some(Action::NewWorkspace));
        assert_eq!(
            bindings.action(b'?'),
            None,
            "no help action; ? is an unknown key"
        );
        assert_eq!(canonical_key(b'{'), b'[');
        assert_eq!(canonical_key(1), 1);
        let mut config = crate::config::Config::default();
        config.bindings.insert("X".into(), Action::CloseTab);
        assert!(
            configured_bindings(&config).is_err(),
            "x and X are the same key"
        );
        let mut config = crate::config::Config {
            prefix: "b".into(),
            ..Default::default()
        };
        config.bindings.insert("B".into(), Action::Detach);
        assert!(configured_bindings(&config).is_err());
    }
}
