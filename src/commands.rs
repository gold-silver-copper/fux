//! The one command registry: configured prefix and bindings, labels, grouping, contextual
//! availability, viewer dispatch and CLI binding output all read from here.

use crate::view::Frame;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const DEFAULT_PREFIX: u8 = 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Action {
    SplitSide,
    SplitStack,
    FocusLeft,
    FocusRight,
    FocusUp,
    FocusDown,
    ClosePane,
    ResizeMode,
    CopyMode,
    NewTab,
    NextTab,
    PreviousTab,
    ChooseTab,
    RenameTab,
    CloseTab,
    ChooseWorkspace,
    NewWorkspace,
    Detach,
    Help,
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

pub const DEFAULT_BINDINGS: &[BindingSpec] = &[
    BindingSpec {
        key: b'|',
        action: Action::SplitSide,
    },
    BindingSpec {
        key: b'-',
        action: Action::SplitStack,
    },
    BindingSpec {
        key: b'x',
        action: Action::ClosePane,
    },
    BindingSpec {
        key: b'r',
        action: Action::ResizeMode,
    },
    BindingSpec {
        key: b'[',
        action: Action::CopyMode,
    },
    BindingSpec {
        key: b'h',
        action: Action::FocusLeft,
    },
    BindingSpec {
        key: b'j',
        action: Action::FocusDown,
    },
    BindingSpec {
        key: b'k',
        action: Action::FocusUp,
    },
    BindingSpec {
        key: b'l',
        action: Action::FocusRight,
    },
    BindingSpec {
        key: b't',
        action: Action::NewTab,
    },
    BindingSpec {
        key: b'n',
        action: Action::NextTab,
    },
    BindingSpec {
        key: b'p',
        action: Action::PreviousTab,
    },
    BindingSpec {
        key: b'w',
        action: Action::ChooseTab,
    },
    BindingSpec {
        key: b',',
        action: Action::RenameTab,
    },
    BindingSpec {
        key: b'X',
        action: Action::CloseTab,
    },
    BindingSpec {
        key: b's',
        action: Action::ChooseWorkspace,
    },
    BindingSpec {
        key: b'S',
        action: Action::NewWorkspace,
    },
    BindingSpec {
        key: b'd',
        action: Action::Detach,
    },
    BindingSpec {
        key: b'?',
        action: Action::Help,
    },
];

impl Action {
    pub const ALL: &'static [Self] = &[
        Self::SplitSide,
        Self::SplitStack,
        Self::FocusLeft,
        Self::FocusRight,
        Self::FocusUp,
        Self::FocusDown,
        Self::ClosePane,
        Self::ResizeMode,
        Self::CopyMode,
        Self::NewTab,
        Self::NextTab,
        Self::PreviousTab,
        Self::ChooseTab,
        Self::RenameTab,
        Self::CloseTab,
        Self::ChooseWorkspace,
        Self::NewWorkspace,
        Self::Detach,
        Self::Help,
    ];

    pub const fn group(self) -> Group {
        match self {
            Self::SplitSide
            | Self::SplitStack
            | Self::ClosePane
            | Self::ResizeMode
            | Self::CopyMode => Group::Panes,
            Self::FocusLeft | Self::FocusRight | Self::FocusUp | Self::FocusDown => Group::Focus,
            Self::NewTab
            | Self::NextTab
            | Self::PreviousTab
            | Self::ChooseTab
            | Self::RenameTab
            | Self::CloseTab => Group::Tabs,
            Self::ChooseWorkspace | Self::NewWorkspace => Group::Workspaces,
            Self::Detach | Self::Help => Group::Session,
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::SplitSide => "split side by side",
            Self::SplitStack => "split stacked",
            Self::FocusLeft => "focus left",
            Self::FocusRight => "focus right",
            Self::FocusUp => "focus up",
            Self::FocusDown => "focus down",
            Self::ClosePane => "close pane",
            Self::ResizeMode => "resize split",
            Self::CopyMode => "history and copy",
            Self::NewTab => "new tab",
            Self::NextTab => "next tab",
            Self::PreviousTab => "previous tab",
            Self::ChooseTab => "choose tab",
            Self::RenameTab => "rename tab",
            Self::CloseTab => "close tab",
            Self::ChooseWorkspace => "choose workspace",
            Self::NewWorkspace => "new workspace",
            Self::Detach => "detach",
            Self::Help => "show bindings",
        }
    }

    /// The obvious contextual restrictions shared by the popup and viewer dispatch. The server
    /// remains authoritative for limits and for changes made by other viewers.
    pub fn unavailable(self, frame: &Frame, workspaces: bool) -> Option<&'static str> {
        let visible = frame.layout.len();
        let live_focus = frame.focused_pane().is_some_and(|pane| pane.exit.is_none());
        match self {
            Self::Help | Self::Detach | Self::NewTab => None,
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
    bindings: BTreeMap<u8, Action>,
}

impl ClientBindings {
    pub fn new(prefix: u8, bindings: impl IntoIterator<Item = (u8, Action)>) -> Self {
        let mut map = BTreeMap::new();
        for (key, action) in bindings {
            if key != prefix {
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

    #[must_use]
    pub fn action(&self, key: u8) -> Option<Action> {
        self.bindings.get(&key).copied()
    }

    #[must_use]
    pub fn key_for(&self, action: Action) -> Option<u8> {
        self.bindings
            .iter()
            .find(|(_, bound)| **bound == action)
            .map(|(key, _)| *key)
    }

    /// Bindings ordered by group then key.
    pub fn entries(&self) -> Vec<(u8, Action)> {
        let mut entries: Vec<_> = self
            .bindings
            .iter()
            .map(|(key, action)| (*key, *action))
            .collect();
        entries.sort_by_key(|(key, action)| (action.group(), *key));
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

/// Resolve the configured registry once for execution, the popup and `fux bindings`.
pub fn configured_bindings(config: &crate::config::Config) -> anyhow::Result<ClientBindings> {
    let prefix =
        key_byte(&config.prefix).ok_or_else(|| anyhow::anyhow!("prefix must encode one byte"))?;
    let mut bindings = BTreeMap::new();
    for (key, action) in &config.bindings {
        let byte =
            key_byte(key).ok_or_else(|| anyhow::anyhow!("binding `{key}` must encode one byte"))?;
        anyhow::ensure!(byte != prefix, "a binding cannot equal the prefix key");
        anyhow::ensure!(
            bindings.insert(byte, *action).is_none(),
            "two binding keys encode the same byte"
        );
    }
    Ok(ClientBindings::new(prefix, bindings))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_action_is_bound_once_by_default_and_labelled() {
        let bindings = ClientBindings::default();
        for action in Action::ALL {
            assert!(bindings.key_for(*action).is_some(), "{action:?} unbound");
            assert!(!action.label().is_empty());
        }
        assert_eq!(bindings.entries().len(), Action::ALL.len());
        assert_eq!(bindings.action(bindings.prefix()), None);
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
        assert!(Action::Help.unavailable(&frame, false).is_none());
        assert!(Action::Detach.unavailable(&frame, false).is_none());
    }
}
