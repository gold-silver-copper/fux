//! One source of identity, labels, groups, and availability for every action.
use crate::{
    control::{Axis, Chooser, Command, Order, Subject},
    model::*,
    navigation,
    protocol::Direction,
};
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
    /// The viewer's current relationships; a detached viewer has no target.
    pub fn of(world: &World, id: Entity) -> Option<Self> {
        Some(Self {
            workspace: viewing(world, id)?,
            tab: on_tab(world, id),
            leaf: focused(world, id),
        })
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

macro_rules! actions {
    (
        $($group:literal: [$($variant:ident $id:literal => $label:literal),* $(,)?]),* $(,)?
    ) => {
        /// The wire form is the snake_case identifier, unchanged from the string API.
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Reflect, Serialize, Deserialize)]
        #[serde(rename_all = "snake_case")]
        pub enum Action {
            $($($variant,)*)*
        }
        /// Bindable actions in help/menu order.
        pub const ALL: &[Action] = &[$($(Action::$variant,)*)*];
        impl Action {
            /// The identifier used in configuration.
            pub const fn id(self) -> &'static str {
                match self {
                    $($(Self::$variant => $id,)*)*
                }
            }
            pub fn group(self) -> &'static str {
                match self {
                    $($(Self::$variant => $group,)*)*
                }
            }
            pub fn label(self) -> &'static str {
                match self {
                    $($(Self::$variant => $label,)*)*
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
}

impl std::fmt::Display for Action {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.id())
    }
}

impl Action {
    /// Pane and focus actions act on a pane, except splits, which can seed an empty tab.
    pub fn needs_pane(self) -> bool {
        matches!(self.group(), "Panes" | "Focus")
            && !matches!(self, Self::SplitHorizontal | Self::SplitVertical)
    }
    /// The command a bound action means for this viewer state, or `None` for
    /// actions that first need a prompt or a confirmation.
    pub fn command(self, target: Target) -> Option<Command> {
        use Action::*;
        let pane = target.leaf.map(Subject::Pane);
        let tab = target.tab.map(Subject::Tab);
        let workspace = Subject::Workspace(target.workspace);
        Some(match self {
            SplitHorizontal => Command::Split {
                axis: Axis::Horizontal,
                program: None,
            },
            SplitVertical => Command::Split {
                axis: Axis::Vertical,
                program: None,
            },
            PaneMenu => Command::Menu { subject: pane? },
            TabMenu => Command::Menu { subject: tab? },
            WorkspaceMenu => Command::Menu { subject: workspace },
            Close => Command::Close { subject: pane? },
            TabClose => Command::Close { subject: tab? },
            WorkspaceClose => Command::Close { subject: workspace },
            Terminate => Command::Terminate,
            Zoom => Command::Zoom,
            GrowWidth | ShrinkWidth => Command::Resize {
                axis: Axis::Horizontal,
                grow: self == GrowWidth,
            },
            GrowHeight | ShrinkHeight => Command::Resize {
                axis: Axis::Vertical,
                grow: self == GrowHeight,
            },
            ReorderPrev => Command::Reorder {
                order: Order::Previous,
            },
            ReorderNext => Command::Reorder { order: Order::Next },
            SwapChoose => Command::Choose {
                chooser: Chooser::SwapTarget,
            },
            SwapLeft | SwapRight | SwapUp | SwapDown => Command::SwapDirection {
                direction: self.direction()?,
            },
            MoveLeft | MoveRight | MoveUp | MoveDown => Command::MoveDirection {
                direction: self.direction()?,
            },
            MoveTab => Command::Choose {
                chooser: Chooser::MoveToTab,
            },
            MoveNewTab => Command::MoveToNewTab { name: None },
            MoveWorkspace => Command::Choose {
                chooser: Chooser::MoveToWorkspace,
            },
            MoveNewWorkspace => Command::MoveToNewWorkspace { name: None },
            CopyMode => Command::CopyMode,
            ScrollUp => Command::Scroll {
                order: Order::Previous,
            },
            ScrollDown => Command::Scroll { order: Order::Next },
            Copy => Command::Copy,
            FocusNext => Command::FocusNext,
            FocusPrevious => Command::FocusPrevious,
            FocusLast => Command::FocusLast,
            FocusLeft | FocusRight | FocusUp | FocusDown => Command::FocusDirection {
                direction: self.direction()?,
            },
            TabNew => Command::TabNew { name: None },
            TabNext => Command::TabNext,
            TabPrevious => Command::TabPrevious,
            TabChoose => Command::Choose {
                chooser: Chooser::Tab,
            },
            TabReorderPrevious => Command::TabReorder {
                order: Order::Previous,
            },
            TabReorderNext => Command::TabReorder { order: Order::Next },
            WorkspaceNew => Command::WorkspaceNew { name: None },
            WorkspaceNext => Command::WorkspaceNext,
            WorkspacePrevious => Command::WorkspacePrevious,
            WorkspaceChoose => Command::Choose {
                chooser: Chooser::Workspace,
            },
            WorkspaceReorderPrevious => Command::WorkspaceReorder {
                order: Order::Previous,
            },
            WorkspaceReorderNext => Command::WorkspaceReorder { order: Order::Next },
            Help => Command::Help,
            Detach => Command::Detach,
            RenamePane | RenameTab | RenameWorkspace | SaveLayout | LoadLayout => return None,
        })
    }
    /// The command a text prompt for this action produces once `value` is typed.
    pub fn with_text(self, target: Target, value: String) -> Option<Command> {
        use Action::*;
        Some(match self {
            RenamePane => Command::Rename {
                subject: Subject::Pane(target.leaf?),
                name: value,
            },
            RenameTab => Command::Rename {
                subject: Subject::Tab(target.tab?),
                name: value,
            },
            RenameWorkspace => Command::Rename {
                subject: Subject::Workspace(target.workspace),
                name: value,
            },
            SaveLayout => Command::SaveLayout {
                workspace: target.workspace,
                path: value,
            },
            LoadLayout => Command::LoadLayout {
                workspace: target.workspace,
                path: value,
                mapping: Vec::new(),
            },
            _ => return None,
        })
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

    #[test]
    fn wire_names_round_trip_and_unknown_names_are_rejected() -> crate::testing::Outcome {
        for action in ALL.iter().copied() {
            let id = action.to_string();
            assert!(id.chars().all(|c| c.is_ascii_lowercase() || c == '_'));
            // The literal in the table and the serde name must never drift.
            let wire = serde_json::Value::String(id);
            assert_eq!(serde_json::to_value(action)?, wire);
            assert_eq!(serde_json::from_value::<Action>(wire)?, action);
        }
        assert_eq!(ALL.len(), 59);
        assert_eq!(Action::SplitHorizontal.to_string(), "split_horizontal");
        assert_eq!(Action::ReorderPrev.to_string(), "reorder_prev");
        assert!(
            serde_json::from_value::<Action>(serde_json::Value::String("nope".into())).is_err()
        );
        assert!(Action::FocusLeft.needs_pane());
        assert!(!Action::SplitVertical.needs_pane());
        assert_eq!(Action::MoveDown.direction(), Some(Direction::Down));
        Ok(())
    }
}
