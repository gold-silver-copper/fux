//! One source of identity, labels, groups, and availability for every action.
use crate::{model::*, navigation, protocol::Direction};
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
        $($group:literal: [$($variant:ident $id:literal => $label:literal),* $(,)?]),* $(,)?
        ; api: [$($api:ident $api_id:literal => $api_label:literal),* $(,)?]
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
        /// Actions the API dispatches but no binding lists.
        const API_ONLY: &[Action] = &[$(Action::$api,)*];
        impl Action {
            /// The identifier used in configuration and over the wire.
            pub const fn id(self) -> &'static str {
                match self {
                    $($(Self::$variant => $id,)*)*
                    $(Self::$api => $api_id,)*
                }
            }
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
        SplitHorizontal "split_horizontal" => "split side by side", SplitVertical "split_vertical" => "split stacked",
        PaneMenu "pane_menu" => "pane actions", RenamePane "rename_pane" => "rename pane", Close "close" => "close pane",
        Terminate "terminate" => "terminate process", Zoom "zoom" => "zoom or restore",
        GrowWidth "grow_width" => "grow width", ShrinkWidth "shrink_width" => "shrink width",
        GrowHeight "grow_height" => "grow height", ShrinkHeight "shrink_height" => "shrink height",
        ReorderPrev "reorder_prev" => "reorder previous", ReorderNext "reorder_next" => "reorder next",
        SwapChoose "swap_choose" => "swap with pane", SwapLeft "swap_left" => "swap left", SwapRight "swap_right" => "swap right",
        SwapUp "swap_up" => "swap up", SwapDown "swap_down" => "swap down",
        MoveLeft "move_left" => "move left", MoveRight "move_right" => "move right", MoveUp "move_up" => "move up", MoveDown "move_down" => "move down",
        MoveTab "move_tab" => "move to tab", MoveNewTab "move_new_tab" => "move to new tab",
        MoveWorkspace "move_workspace" => "move to workspace", MoveNewWorkspace "move_new_workspace" => "move to new workspace",
        CopyMode "copy_mode" => "history and selection", ScrollUp "scroll_up" => "scroll older output",
        ScrollDown "scroll_down" => "scroll newer output", Copy "copy" => "copy visible text"
    ],
    "Focus": [FocusNext "focus_next" => "next pane", FocusPrevious "focus_previous" => "previous pane", FocusLast "focus_last" => "last pane",
        FocusLeft "focus_left" => "focus left", FocusRight "focus_right" => "focus right", FocusUp "focus_up" => "focus up", FocusDown "focus_down" => "focus down"],
    "Tabs": [TabNew "tab_new" => "new tab", TabNext "tab_next" => "next tab", TabPrevious "tab_previous" => "previous tab",
        TabChoose "tab_choose" => "choose tab", RenameTab "rename_tab" => "rename tab", TabClose "tab_close" => "close tab",
        TabMenu "tab_menu" => "tab actions", TabReorderPrevious "tab_reorder_previous" => "reorder tab previous", TabReorderNext "tab_reorder_next" => "reorder tab next"],
    "Workspaces": [WorkspaceNew "workspace_new" => "new workspace", WorkspaceNext "workspace_next" => "next workspace",
        WorkspacePrevious "workspace_previous" => "previous workspace", WorkspaceChoose "workspace_choose" => "choose workspace",
        RenameWorkspace "rename_workspace" => "rename workspace", WorkspaceClose "workspace_close" => "close workspace", WorkspaceMenu "workspace_menu" => "workspace actions",
        WorkspaceReorderPrevious "workspace_reorder_previous" => "reorder workspace previous", WorkspaceReorderNext "workspace_reorder_next" => "reorder workspace next"],
    "Session": [SaveLayout "save_layout" => "save layout", LoadLayout "load_layout" => "load layout", Help "help" => "command help", Detach "detach" => "detach"]
    ; api: [Focus "focus" => "focus", TabSelect "tab_select" => "tab select", WorkspaceSelect "workspace_select" => "workspace select", Swap "swap" => "swap"]
}

impl std::str::FromStr for Action {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        ALL.iter()
            .chain(API_ONLY)
            .copied()
            .find(|action| action.id() == s)
            .ok_or_else(|| format!("unknown action {s}"))
    }
}
impl std::fmt::Display for Action {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.id())
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
    /// The direction of a directional focus, swap or move action.
    pub fn direction(self) -> Option<Direction> {
        use Action::*;
        match self {
            FocusLeft | SwapLeft | MoveLeft => Some(Direction::Left),
            FocusRight | SwapRight | MoveRight => Some(Direction::Right),
            FocusUp | SwapUp | MoveUp => Some(Direction::Up),
            FocusDown | SwapDown | MoveDown => Some(Direction::Down),
            _ => None,
        }
    }
}

pub const TARGET_GONE: &str = "target no longer exists here";

pub fn unavailable(world: &World, target: Target, action: Action) -> Option<&'static str> {
    use Action::*;
    if !target.valid(world) {
        return Some(TARGET_GONE);
    }
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
    use crate::testing::*;

    #[test]
    fn wire_names_round_trip_and_unknown_names_are_rejected() -> crate::testing::Outcome {
        for action in ALL.iter().chain(API_ONLY).copied() {
            let id = action.to_string();
            assert_eq!(id.parse::<Action>()?, action);
            assert!(id.chars().all(|c| c.is_ascii_lowercase() || c == '_'));
            // The literal in the table and the serde name must never drift.
            assert_eq!(serde_json::to_value(action)?, serde_json::Value::String(id));
        }
        assert_eq!(API_ONLY.len(), 4);
        assert_eq!(Action::SplitHorizontal.to_string(), "split_horizontal");
        assert_eq!(Action::ReorderPrev.to_string(), "reorder_prev");
        assert_eq!(
            "nope".parse::<Action>().err().need()?,
            "unknown action nope"
        );
        assert_eq!(Action::Focus.group(), "Other");
        assert_eq!(Action::TabSelect.label(), "tab select");
        assert!(Action::FocusLeft.needs_pane());
        assert!(!Action::SplitVertical.needs_pane());
        assert_eq!(Action::MoveTab.target_kind(), TargetKind::Tab);
        assert_eq!(Action::SaveLayout.target_kind(), TargetKind::Workspace);
        assert_eq!(Action::Swap.target_kind(), TargetKind::Pane);
        assert_eq!(Action::Help.target_kind(), TargetKind::Any);
        assert_eq!(Action::MoveDown.direction(), Some(Direction::Down));
        Ok(())
    }
}
