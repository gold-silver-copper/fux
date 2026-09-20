//! One source of identity, labels, groups, and availability for every action.
use crate::{model::*, navigation};
use bevy_ecs::prelude::*;
use bevy_reflect::Reflect;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Target {
    pub workspace: Entity,
    pub tab: Option<Entity>,
    pub leaf: Option<Entity>,
}
impl Target {
    pub fn viewer(v: &Viewer) -> Self {
        Self {
            workspace: v.workspace,
            tab: v.tab,
            leaf: v.focus,
        }
    }
    pub fn valid(self, world: &World) -> bool {
        world.get::<Workspace>(self.workspace).is_some()
            && self
                .tab
                .is_none_or(|tab| navigation::tabs(world, self.workspace).contains(&tab))
            && self.leaf.is_none_or(|leaf| {
                self.tab
                    .is_some_and(|tab| navigation::leaves(world, tab).contains(&leaf))
            })
    }
}

/// The entity kind an explicit `Control.target` may name for an action.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TargetKind {
    Any,
    Pane,
    Tab,
    Workspace,
}

macro_rules! actions {
    (
        $($group:literal: [$($variant:ident => $label:literal),* $(,)?]),* $(,)?
        ; api: [$($api:ident => $api_label:literal),* $(,)?]
    ) => {
        /// The wire form is the snake_case identifier, unchanged from the string API.
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Reflect, Serialize, Deserialize)]
        #[serde(rename_all = "snake_case")]
        pub enum Action {
            $($($variant,)*)*
            $($api,)*
        }
        /// Bindable actions in help/menu order. API-only actions are excluded.
        pub const ALL: &[Action] = &[$($(Action::$variant,)*)*];
        impl Action {
            pub fn group(self) -> &'static str {
                match self {
                    $($(Self::$variant => $group,)*)*
                    $(Self::$api => "Other",)*
                }
            }
            pub fn label(self) -> &'static str {
                match self {
                    $($(Self::$variant => $label,)*)*
                    $(Self::$api => $api_label,)*
                }
            }
        }
    }
}
actions! {
    "Panes": [
        SplitHorizontal => "split side by side", SplitVertical => "split stacked",
        PaneMenu => "pane actions", RenamePane => "rename pane", Close => "close pane",
        Terminate => "terminate process", Zoom => "zoom or restore",
        GrowWidth => "grow width", ShrinkWidth => "shrink width",
        GrowHeight => "grow height", ShrinkHeight => "shrink height",
        ReorderPrev => "reorder previous", ReorderNext => "reorder next",
        SwapChoose => "swap with pane", SwapLeft => "swap left", SwapRight => "swap right",
        SwapUp => "swap up", SwapDown => "swap down",
        MoveLeft => "move left", MoveRight => "move right", MoveUp => "move up", MoveDown => "move down",
        MoveTab => "move to tab", MoveNewTab => "move to new tab",
        MoveWorkspace => "move to workspace", MoveNewWorkspace => "move to new workspace",
        CopyMode => "history and selection", ScrollUp => "scroll older output",
        ScrollDown => "scroll newer output", Copy => "copy visible text"
    ],
    "Focus": [FocusNext => "next pane", FocusPrevious => "previous pane", FocusLast => "last pane",
        FocusLeft => "focus left", FocusRight => "focus right", FocusUp => "focus up", FocusDown => "focus down"],
    "Tabs": [TabNew => "new tab", TabNext => "next tab", TabPrevious => "previous tab",
        TabChoose => "choose tab", RenameTab => "rename tab", TabClose => "close tab",
        TabMenu => "tab actions", TabReorderPrevious => "reorder tab previous", TabReorderNext => "reorder tab next"],
    "Workspaces": [WorkspaceNew => "new workspace", WorkspaceNext => "next workspace",
        WorkspacePrevious => "previous workspace", WorkspaceChoose => "choose workspace",
        RenameWorkspace => "rename workspace", WorkspaceClose => "close workspace", WorkspaceMenu => "workspace actions",
        WorkspaceReorderPrevious => "reorder workspace previous", WorkspaceReorderNext => "reorder workspace next"],
    "Session": [SaveLayout => "save layout", LoadLayout => "load layout", Help => "command help", Detach => "detach"]
    ; api: [Focus => "focus", TabSelect => "tab select", WorkspaceSelect => "workspace select", Swap => "swap"]
}

impl std::str::FromStr for Action {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        serde_json::from_value(serde_json::Value::String(s.to_owned()))
            .map_err(|_| format!("unknown action {s}"))
    }
}
impl std::fmt::Display for Action {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match serde_json::to_value(self) {
            Ok(serde_json::Value::String(id)) => f.write_str(&id),
            _ => write!(f, "{self:?}"),
        }
    }
}

impl Action {
    pub fn target_kind(self) -> TargetKind {
        use Action::*;
        match self {
            TabClose | RenameTab | TabMenu | TabSelect | TabReorderPrevious | TabReorderNext
            | MoveTab => TargetKind::Tab,
            WorkspaceClose
            | RenameWorkspace
            | WorkspaceMenu
            | WorkspaceSelect
            | WorkspaceReorderPrevious
            | WorkspaceReorderNext
            | MoveWorkspace
            | SaveLayout
            | LoadLayout => TargetKind::Workspace,
            Close | RenamePane | Terminate | Focus | Zoom | Copy | CopyMode | Swap => {
                TargetKind::Pane
            }
            _ => TargetKind::Any,
        }
    }
    /// Pane and focus actions act on a pane, except splits, which can seed an empty tab.
    pub fn needs_pane(self) -> bool {
        matches!(self.group(), "Panes" | "Focus")
            && !matches!(self, Self::SplitHorizontal | Self::SplitVertical)
    }
    /// The direction word of a directional focus, swap or move action.
    pub fn direction(self) -> Option<&'static str> {
        use Action::*;
        match self {
            FocusLeft | SwapLeft | MoveLeft => Some("left"),
            FocusRight | SwapRight | MoveRight => Some("right"),
            FocusUp | SwapUp | MoveUp => Some("up"),
            FocusDown | SwapDown | MoveDown => Some("down"),
            _ => None,
        }
    }
}

/// `None` is an unrecognized custom action: only its target validity is checked.
pub fn unavailable(world: &World, target: Target, action: Option<Action>) -> Option<&'static str> {
    use Action::*;
    if !target.valid(world) {
        return Some("target no longer exists here");
    }
    let action = action?;
    if action == Copy
        && world
            .get_resource::<crate::assets::Settings>()
            .is_some_and(|s| s.clipboard == crate::assets::ClipboardPolicy::Disabled)
    {
        return Some("clipboard disabled; configure clipboard: write-only");
    }
    if action.needs_pane() && target.leaf.is_none() {
        return Some("no pane");
    }
    if action.group() == "Tabs" && target.tab.is_none() {
        return Some("no tab");
    }
    if matches!(
        action,
        TabNext | TabPrevious | TabReorderPrevious | TabReorderNext
    ) && navigation::tabs(world, target.workspace).len() < 2
    {
        return Some("only one tab");
    }
    if matches!(
        action,
        SwapChoose
            | SwapLeft
            | SwapRight
            | SwapUp
            | SwapDown
            | MoveLeft
            | MoveRight
            | MoveUp
            | MoveDown
            | FocusNext
            | FocusPrevious
            | FocusLast
            | ReorderPrev
            | ReorderNext
    ) && target
        .tab
        .is_none_or(|tab| navigation::leaves(world, tab).len() < 2)
    {
        return Some("only one pane");
    }
    if action == Terminate
        && target
            .leaf
            .and_then(|leaf| world.get::<PaneView>(leaf))
            .is_none_or(|view| world.get::<crate::terminal::Terminal>(view.pane).is_none())
    {
        return Some("process is not running");
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_names_round_trip_and_unknown_names_are_rejected() {
        for action in ALL.iter().copied().chain([
            Action::Focus,
            Action::TabSelect,
            Action::WorkspaceSelect,
            Action::Swap,
        ]) {
            let id = action.to_string();
            assert_eq!(id.parse::<Action>().unwrap(), action);
            assert!(id.chars().all(|c| c.is_ascii_lowercase() || c == '_'));
        }
        assert_eq!(Action::SplitHorizontal.to_string(), "split_horizontal");
        assert_eq!(Action::ReorderPrev.to_string(), "reorder_prev");
        assert_eq!("nope".parse::<Action>().unwrap_err(), "unknown action nope");
        assert_eq!(Action::Focus.group(), "Other");
        assert_eq!(Action::TabSelect.label(), "tab select");
        assert!(Action::FocusLeft.needs_pane());
        assert!(!Action::SplitVertical.needs_pane());
        assert_eq!(Action::MoveTab.target_kind(), TargetKind::Tab);
        assert_eq!(Action::SaveLayout.target_kind(), TargetKind::Workspace);
        assert_eq!(Action::Swap.target_kind(), TargetKind::Pane);
        assert_eq!(Action::Help.target_kind(), TargetKind::Any);
        assert_eq!(Action::MoveDown.direction(), Some("down"));
    }
}
