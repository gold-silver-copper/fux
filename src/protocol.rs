use bevy_ecs::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PaneRect {
    /// IDs in the authoritative server World, not the presentation World.
    pub leaf: Entity,
    pub pane: Entity,
    /// Content rectangle in zero-based viewer cells; no border or title inset.
    /// The final viewer row is chrome and never belongs to a pane.
    pub x: u16,
    pub y: u16,
    pub width: u16,
    pub height: u16,
}

#[derive(Debug, Clone, Serialize, Deserialize, bevy_reflect::Reflect)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Input {
    /// Canonical key: single Unicode character, enter, tab, escape, backspace,
    /// left/right/up/down, home/end, pageup/pagedown, delete, insert, f1..f12.
    Key {
        key: String,
        ctrl: bool,
        alt: bool,
        shift: bool,
    },
    /// Capture the current input owner before a fragmented bracketed paste.
    PasteBegin,
    Paste {
        text: String,
    },
    /// kind: press/release/move/scrollup/scrolldown; button: 0 left, 1 middle, 2 right.
    Mouse {
        action: String,
        button: u8,
        x: u16,
        y: u16,
        ctrl: bool,
        alt: bool,
        shift: bool,
    },
    Resize {
        rows: u16,
        cols: u16,
    },
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Frame {
    pub paint: String,
    pub detach: bool,
}
