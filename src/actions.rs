//! One source of labels, groups, and availability for help and interactive actions.
use crate::{model::*, navigation};
use bevy_ecs::prelude::*;

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

pub struct Action {
    pub id: &'static str,
    pub group: &'static str,
    pub label: &'static str,
}
macro_rules! actions {
    ($($group:literal: [$($id:literal => $label:literal),* $(,)?]),* $(,)?) => {
        pub const ALL: &[Action] = &[$($(Action { id: $id, group: $group, label: $label },)*)*];
    }
}
actions! {
    "Panes": [
        "split_horizontal" => "split side by side", "split_vertical" => "split stacked",
        "pane_menu" => "pane actions", "rename_pane" => "rename pane", "close" => "close pane",
        "terminate" => "terminate process", "zoom" => "zoom or restore",
        "grow_width" => "grow width", "shrink_width" => "shrink width",
        "grow_height" => "grow height", "shrink_height" => "shrink height",
        "reorder_prev" => "reorder previous", "reorder_next" => "reorder next",
        "swap_choose" => "swap with pane", "swap_left" => "swap left", "swap_right" => "swap right",
        "swap_up" => "swap up", "swap_down" => "swap down",
        "move_left" => "move left", "move_right" => "move right", "move_up" => "move up", "move_down" => "move down",
        "move_tab" => "move to tab", "move_new_tab" => "move to new tab",
        "move_workspace" => "move to workspace", "move_new_workspace" => "move to new workspace",
        "copy_mode" => "history and selection", "scroll_up" => "scroll older output",
        "scroll_down" => "scroll newer output", "copy" => "copy visible text"
    ],
    "Focus": ["focus_next" => "next pane", "focus_previous" => "previous pane", "focus_last" => "last pane",
        "focus_left" => "focus left", "focus_right" => "focus right", "focus_up" => "focus up", "focus_down" => "focus down"],
    "Tabs": ["tab_new" => "new tab", "tab_next" => "next tab", "tab_previous" => "previous tab",
        "tab_choose" => "choose tab", "rename_tab" => "rename tab", "tab_close" => "close tab",
        "tab_menu" => "tab actions", "tab_reorder_previous" => "reorder tab previous", "tab_reorder_next" => "reorder tab next"],
    "Workspaces": ["workspace_new" => "new workspace", "workspace_next" => "next workspace",
        "workspace_previous" => "previous workspace", "workspace_choose" => "choose workspace",
        "rename_workspace" => "rename workspace", "workspace_close" => "close workspace", "workspace_menu" => "workspace actions",
        "workspace_reorder_previous" => "reorder workspace previous", "workspace_reorder_next" => "reorder workspace next"],
    "Session": ["save_layout" => "save layout", "load_layout" => "load layout", "help" => "command help", "detach" => "detach"]
}

pub fn metadata(id: &str) -> Option<&'static Action> {
    ALL.iter().find(|a| a.id == id)
}

pub fn unavailable(world: &World, target: Target, action: &str) -> Option<&'static str> {
    if !target.valid(world) {
        return Some("target no longer exists here");
    }
    if action == "copy"
        && world
            .get_resource::<crate::assets::Settings>()
            .is_some_and(|s| s.clipboard == crate::assets::ClipboardPolicy::Disabled)
    {
        return Some("clipboard disabled; configure clipboard: write-only");
    }
    let needs_pane = metadata(action).is_some_and(|a| a.group == "Panes" || a.group == "Focus")
        && !matches!(action, "split_horizontal" | "split_vertical");
    if needs_pane && target.leaf.is_none() {
        return Some("no pane");
    }
    if metadata(action).is_some_and(|a| a.group == "Tabs") && target.tab.is_none() {
        return Some("no tab");
    }
    if matches!(
        action,
        "tab_next" | "tab_previous" | "tab_reorder_previous" | "tab_reorder_next"
    ) && navigation::tabs(world, target.workspace).len() < 2
    {
        return Some("only one tab");
    }
    if (action.starts_with("swap_")
        || action.starts_with("move_")
            && matches!(action, "move_left" | "move_right" | "move_up" | "move_down")
        || matches!(
            action,
            "focus_next" | "focus_previous" | "focus_last" | "reorder_prev" | "reorder_next"
        ))
        && target
            .tab
            .is_none_or(|tab| navigation::leaves(world, tab).len() < 2)
    {
        return Some("only one pane");
    }
    if action == "terminate"
        && target
            .leaf
            .and_then(|leaf| world.get::<PaneView>(leaf))
            .is_none_or(|view| world.get::<crate::terminal::Terminal>(view.pane).is_none())
    {
        return Some("process is not running");
    }
    None
}
