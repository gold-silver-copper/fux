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
    TabPicker,
    RenameTab,
    ResizeMode,
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
    TabPicker,
    RenameTab,
    ResizeMode,
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
    BindingSpec {
        key: b'w',
        action: BuiltinAction::TabPicker,
        command: Command::TabPicker,
        description: "choose tab",
    },
    BindingSpec {
        key: b',',
        action: BuiltinAction::RenameTab,
        command: Command::RenameTab,
        description: "rename tab",
    },
    BindingSpec {
        key: b'r',
        action: BuiltinAction::ResizeMode,
        command: Command::ResizeMode,
        description: "resize mode",
    },
];

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub enum CommandGroup {
    Panes,
    Focus,
    Tabs,
    Session,
    Custom,
}
impl CommandGroup {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Panes => "Panes",
            Self::Focus => "Focus",
            Self::Tabs => "Tabs",
            Self::Session => "Session",
            Self::Custom => "Custom",
        }
    }
}

impl BuiltinAction {
    pub const fn group(self) -> CommandGroup {
        match self {
            Self::FocusLeft | Self::FocusRight | Self::FocusUp | Self::FocusDown => {
                CommandGroup::Focus
            }
            Self::NewTab
            | Self::NextTab
            | Self::PreviousTab
            | Self::TabPicker
            | Self::RenameTab => CommandGroup::Tabs,
            Self::Detach | Self::WorkspacePicker | Self::Help => CommandGroup::Session,
            Self::SplitHorizontal
            | Self::SplitVertical
            | Self::ClosePane
            | Self::NewPane
            | Self::Zoom
            | Self::CopyMode
            | Self::ResizeMode => CommandGroup::Panes,
        }
    }
    /// Obvious context restrictions shared by hints and viewer dispatch. The host
    /// remains authoritative for resources and changes made by other viewers.
    pub fn unavailable(
        self,
        state: &crate::state::WorkspaceState,
        manager: bool,
    ) -> Option<&'static str> {
        let tab = state
            .tabs()
            .iter()
            .find(|tab| Some(tab.id) == state.active_tab());
        let popup = state.popups().iter().max_by_key(|popup| popup.z_index);
        let pane = popup
            .map(|popup| popup.pane)
            .or_else(|| tab.map(|tab| tab.focused))
            .and_then(|pane| state.pane(pane));
        match self {
            Self::Help | Self::Detach => None,
            Self::WorkspacePicker => {
                (!manager).then_some("No workspace manager for this attachment")
            }
            Self::ClosePane | Self::CopyMode => pane
                .filter(|pane| pane.exit_status.is_none())
                .is_none()
                .then_some("No live pane"),
            _ if popup.is_some() => Some("Close the popup first"),
            Self::NewTab => None,
            Self::NextTab | Self::PreviousTab => (state.tabs().len() < 2).then_some("Only one tab"),
            Self::TabPicker | Self::RenameTab => tab.is_none().then_some("No active tab"),
            Self::FocusLeft
            | Self::FocusRight
            | Self::FocusUp
            | Self::FocusDown
            | Self::ResizeMode => tab
                .is_none_or(|tab| tab.layout.leaves().len() < 2)
                .then_some("No split to adjust"),
            Self::SplitHorizontal | Self::SplitVertical | Self::NewPane | Self::Zoom => {
                pane.is_none().then_some("No active pane")
            }
        }
    }

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
    pub fn group(&self) -> CommandGroup {
        DEFAULT_BINDINGS
            .iter()
            .find(|spec| spec.command == *self)
            .map_or(CommandGroup::Custom, |spec| spec.action.group())
    }
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

/// Resolve the configured registry once for both execution and command discovery.
/// External command arguments remain local execution data; help uses the command's safe label.
pub fn configured_bindings(
    config: &crate::config::Config,
) -> anyhow::Result<std::collections::BTreeMap<u8, Command>> {
    let mut bindings = std::collections::BTreeMap::new();
    for (key, binding) in &config.bindings {
        let byte =
            key_byte(key).ok_or_else(|| anyhow::anyhow!("binding `{key}` must encode one byte"))?;
        let command = match binding {
            crate::config::Binding::Builtin { builtin } => builtin
                .command()
                .ok_or_else(|| anyhow::anyhow!("unregistered binding action"))?,
            crate::config::Binding::External { external } => {
                Command::External(external.argv.clone())
            }
        };
        anyhow::ensure!(
            bindings.insert(byte, command).is_none(),
            "bindings must not alias the same byte"
        );
    }
    let prefix =
        key_byte(&config.prefix).ok_or_else(|| anyhow::anyhow!("prefix must encode one byte"))?;
    anyhow::ensure!(
        !bindings.contains_key(&prefix),
        "a binding cannot equal the prefix byte"
    );
    Ok(bindings)
}

/// Fixed-size viewer shortcuts published by the workspace alongside its state.
/// Bitsets cover every possible single-byte binding without peer-controlled allocation.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ClientBindings {
    prefix: u8,
    detach: [u8; 32],
    workspace_picker: [u8; 32],
    #[serde(default)]
    commands: [u64; 32],
}

impl ClientBindings {
    pub fn new<'a>(prefix: u8, bindings: impl IntoIterator<Item = (u8, &'a Command)>) -> Self {
        let mut policy = Self {
            prefix,
            detach: [0; 32],
            workspace_picker: [0; 32],
            commands: [0; 32],
        };
        for (key, command) in bindings {
            let code = DEFAULT_BINDINGS
                .iter()
                .position(|spec| spec.command == *command)
                .and_then(|index| u8::try_from(index + 1).ok())
                .unwrap_or(255);
            if let Some(slot) = policy.commands.get_mut(usize::from(key / 8)) {
                *slot |= u64::from(code) << (u32::from(key % 8) * 8);
            }
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

    pub fn is_bound(&self, key: u8) -> bool {
        self.code(key) != 0 || self.action(key).is_some()
    }

    pub fn description(&self, key: u8) -> &'static str {
        self.action(key)
            .map_or("external command", BuiltinAction::description)
    }

    pub fn entries(&self) -> impl Iterator<Item = (u8, &'static str)> + '_ {
        (0..=u8::MAX)
            .filter(|key| self.is_bound(*key))
            .map(|key| (key, self.description(key)))
    }

    fn code(&self, key: u8) -> u8 {
        self.commands
            .get(usize::from(key / 8))
            .map_or(0, |slot| ((slot >> (u32::from(key % 8) * 8)) & 255) as u8)
    }

    pub fn action(&self, key: u8) -> Option<BuiltinAction> {
        if let Some(spec) = self
            .code(key)
            .checked_sub(1)
            .and_then(|index| DEFAULT_BINDINGS.get(usize::from(index)))
        {
            return Some(spec.action);
        }
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
            Self::TabPicker
            | Self::RenameTab
            | Self::ResizeMode
            | Self::CopyMode
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
