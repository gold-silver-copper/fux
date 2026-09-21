#[cfg(test)]
mod tests;

use crate::{
    interaction::MoveTo,
    protocol::{Direction, Input},
};
use bevy_ecs::prelude::*;
use bevy_reflect::{Reflect, ReflectDeserialize, ReflectSerialize};
use serde::{Deserialize, Serialize};

/// Trigger through stock world.trigger_event; errors are exposed in Viewer.notice.
/// A `command` that is not a `Command` is rejected when the request deserializes.
#[derive(EntityEvent, Reflect, Clone, Serialize, Deserialize)]
#[reflect(Event, Serialize, Deserialize)]
pub struct Control {
    #[event_target]
    pub viewer: Entity,
    pub command: Command,
}

#[derive(EntityEvent, Reflect, Clone, Serialize, Deserialize)]
#[reflect(Event, Serialize, Deserialize)]
pub struct UserInput {
    #[event_target]
    pub viewer: Entity,
    pub input: Input,
}

#[derive(Event, Reflect, Clone, Serialize, Deserialize)]
#[reflect(Event, Serialize, Deserialize)]
pub struct Shutdown;

/// One tagged command. Every field is what the command needs and nothing else,
/// so a request cannot combine a subject, a value and a mapping that disagree.
#[derive(Reflect, Clone, Debug, PartialEq, Serialize, Deserialize)]
#[reflect(Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum Command {
    Split {
        axis: Axis,
        program: Option<String>,
    },
    Close {
        subject: Subject,
    },
    Terminate,
    Zoom,
    Rename {
        subject: Subject,
        name: String,
    },
    Resize {
        axis: Axis,
        grow: bool,
    },
    ReorderPane {
        order: Order,
    },
    Swap {
        with: Entity,
    },
    SwapDirection {
        direction: Direction,
    },
    MoveDirection {
        direction: Direction,
    },
    Move {
        to: MoveTo,
    },
    CopyMode,
    /// `Previous` shows older output, `Next` newer.
    Scroll {
        order: Order,
    },
    Copy,
    Focus {
        pane: Entity,
    },
    FocusNext,
    FocusPrevious,
    FocusLast,
    FocusDirection {
        direction: Direction,
    },
    TabNew {
        name: Option<String>,
    },
    Select {
        scope: Scope,
        entity: Entity,
    },
    Next {
        scope: Scope,
    },
    Previous {
        scope: Scope,
    },
    Reorder {
        scope: Scope,
        order: Order,
    },
    WorkspaceNew {
        name: Option<String>,
    },
    SaveLayout {
        workspace: Entity,
        path: String,
    },
    LoadLayout {
        workspace: Entity,
        path: String,
        mapping: Vec<(Entity, Entity)>,
    },
    Help,
    Detach,
    Menu {
        subject: Subject,
    },
    Choose {
        chooser: Chooser,
    },
}

#[derive(Reflect, Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[reflect(Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Subject {
    Pane(Entity),
    Tab(Entity),
    Workspace(Entity),
}

#[derive(Reflect, Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[reflect(Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Axis {
    Horizontal,
    Vertical,
}

#[derive(Reflect, Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[reflect(Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Order {
    Previous,
    Next,
}

#[derive(Reflect, Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[reflect(Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Scope {
    Tab,
    Workspace,
}

/// Interactive lists: the destination is chosen from what exists now.
#[derive(Reflect, Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[reflect(Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Chooser {
    Tab,
    Workspace,
    SwapTarget,
    MoveToTab,
    MoveToWorkspace,
}
