//! Application commands and the authoritative default binding/help registry.
use crate::state::Direction;
use serde::{Deserialize, Serialize};

pub const DEFAULT_PREFIX: u8 = 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum BuiltinAction {
    SplitHorizontal,
    SplitVertical,
    FocusLeft,
    FocusRight,
    FocusUp,
    FocusDown,
    ClosePane,
    NewPane,
    NewTab,
    NextTab,
    PreviousTab,
    Zoom,
    CopyMode,
    Detach,
    WorkspacePicker,
    Help,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Command {
    SplitHorizontal,
    SplitVertical,
    Focus(Direction),
    Close,
    NewPane,
    NewTab,
    NextTab,
    PreviousTab,
    Zoom,
    CopyMode,
    Detach,
    WorkspacePicker,
    Help,
    External(Vec<String>),
}

pub struct BindingSpec {
    pub key: u8,
    pub action: BuiltinAction,
    pub command: Command,
    pub description: &'static str,
}

pub const DEFAULT_BINDINGS: &[BindingSpec] = &[
    BindingSpec {
        key: b'|',
        action: BuiltinAction::SplitHorizontal,
        command: Command::SplitHorizontal,
        description: "split side by side",
    },
    BindingSpec {
        key: b'-',
        action: BuiltinAction::SplitVertical,
        command: Command::SplitVertical,
        description: "split stacked",
    },
    BindingSpec {
        key: b'h',
        action: BuiltinAction::FocusLeft,
        command: Command::Focus(Direction::Left),
        description: "focus left",
    },
    BindingSpec {
        key: b'j',
        action: BuiltinAction::FocusDown,
        command: Command::Focus(Direction::Down),
        description: "focus down",
    },
    BindingSpec {
        key: b'k',
        action: BuiltinAction::FocusUp,
        command: Command::Focus(Direction::Up),
        description: "focus up",
    },
    BindingSpec {
        key: b'l',
        action: BuiltinAction::FocusRight,
        command: Command::Focus(Direction::Right),
        description: "focus right",
    },
    BindingSpec {
        key: b'x',
        action: BuiltinAction::ClosePane,
        command: Command::Close,
        description: "close pane",
    },
    BindingSpec {
        key: b'c',
        action: BuiltinAction::NewPane,
        command: Command::NewPane,
        description: "new pane",
    },
    BindingSpec {
        key: b't',
        action: BuiltinAction::NewTab,
        command: Command::NewTab,
        description: "new tab",
    },
    BindingSpec {
        key: b'n',
        action: BuiltinAction::NextTab,
        command: Command::NextTab,
        description: "next tab",
    },
    BindingSpec {
        key: b'p',
        action: BuiltinAction::PreviousTab,
        command: Command::PreviousTab,
        description: "previous tab",
    },
    BindingSpec {
        key: b'z',
        action: BuiltinAction::Zoom,
        command: Command::Zoom,
        description: "toggle zoom",
    },
    BindingSpec {
        key: b'[',
        action: BuiltinAction::CopyMode,
        command: Command::CopyMode,
        description: "copy mode",
    },
    BindingSpec {
        key: b'd',
        action: BuiltinAction::Detach,
        command: Command::Detach,
        description: "detach viewer",
    },
    BindingSpec {
        key: b's',
        action: BuiltinAction::WorkspacePicker,
        command: Command::WorkspacePicker,
        description: "choose workspace",
    },
    BindingSpec {
        key: b'?',
        action: BuiltinAction::Help,
        command: Command::Help,
        description: "show bindings",
    },
];

impl BuiltinAction {
    pub fn command(self) -> Option<Command> {
        DEFAULT_BINDINGS
            .iter()
            .find(|spec| spec.action == self)
            .map(|spec| spec.command.clone())
    }
    pub fn description(self) -> &'static str {
        DEFAULT_BINDINGS
            .iter()
            .find(|spec| spec.action == self)
            .map_or("unregistered action", |spec| spec.description)
    }
}

impl Command {
    pub fn description(&self) -> &'static str {
        DEFAULT_BINDINGS
            .iter()
            .find(|spec| spec.command == *self)
            .map_or("external command", |spec| spec.description)
    }
}

pub fn key_name(key: u8) -> String {
    match key {
        0 => "C-@".to_owned(),
        1..=26 => format!("C-{}", char::from(key + b'a' - 1)),
        27..=31 => format!("C-{}", char::from(key + b'@')),
        32 => "Space".to_owned(),
        127 => "DEL".to_owned(),
        128..=255 => format!("0x{key:02x}"),
        _ => char::from(key).to_string(),
    }
}

/// Fixed-size viewer shortcuts published by the workspace alongside its state.
/// Bitsets cover every possible single-byte binding without peer-controlled allocation.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ClientBindings {
    prefix: u8,
    detach: [u8; 32],
    workspace_picker: [u8; 32],
}

impl ClientBindings {
    pub fn new<'a>(prefix: u8, bindings: impl IntoIterator<Item = (u8, &'a Command)>) -> Self {
        let mut policy = Self {
            prefix,
            detach: [0; 32],
            workspace_picker: [0; 32],
        };
        for (key, command) in bindings {
            let bits = match command {
                Command::Detach => &mut policy.detach,
                Command::WorkspacePicker => &mut policy.workspace_picker,
                _ => continue,
            };
            if let Some(slot) = bits.get_mut(usize::from(key / 8)) {
                *slot |= 1 << (key % 8);
            }
        }
        policy
    }

    pub fn prefix(&self) -> u8 {
        self.prefix
    }

    pub fn action(&self, key: u8) -> Option<BuiltinAction> {
        let contains = |bits: &[u8; 32]| {
            bits.get(usize::from(key / 8))
                .is_some_and(|slot| slot & (1 << (key % 8)) != 0)
        };
        if contains(&self.detach) {
            Some(BuiltinAction::Detach)
        } else if contains(&self.workspace_picker) {
            Some(BuiltinAction::WorkspacePicker)
        } else {
            None
        }
    }
}

impl Default for ClientBindings {
    fn default() -> Self {
        Self::new(
            DEFAULT_PREFIX,
            DEFAULT_BINDINGS
                .iter()
                .map(|spec| (spec.key, &spec.command)),
        )
    }
}

impl Command {
    /// Translate viewer actions into the same typed requests used by CLI/control clients.
    pub fn request(&self, focused: Option<u32>) -> Option<crate::control::Request> {
        use crate::control::{Axis, FocusTarget, Request, TabAction};
        Some(match self {
            Self::SplitHorizontal | Self::SplitVertical => Request::Split {
                id: 0,
                axis: if *self == Self::SplitHorizontal {
                    Axis::Horizontal
                } else {
                    Axis::Vertical
                },
                target: None,
                argv: Vec::new(),
                env: Default::default(),
            },
            Self::NewPane => Request::New {
                id: 0,
                cwd: None,
                argv: Vec::new(),
                env: Default::default(),
            },
            Self::Focus(direction) => Request::Focus {
                id: 0,
                target: match direction {
                    Direction::Left => FocusTarget::Left,
                    Direction::Right => FocusTarget::Right,
                    Direction::Up => FocusTarget::Up,
                    Direction::Down => FocusTarget::Down,
                },
            },
            Self::Close => Request::Kill {
                id: 0,
                pane: focused?,
            },
            Self::Zoom => Request::Zoom { id: 0, pane: None },
            Self::NewTab => Request::Tab {
                id: 0,
                action: TabAction::New { name: None },
            },
            Self::NextTab => Request::Tab {
                id: 0,
                action: TabAction::Next,
            },
            Self::PreviousTab => Request::Tab {
                id: 0,
                action: TabAction::Previous,
            },
            Self::CopyMode
            | Self::Detach
            | Self::WorkspacePicker
            | Self::Help
            | Self::External(_) => return None,
        })
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
    (value.len() == 1)
        .then(|| value.as_bytes().first().copied())
        .flatten()
}
