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

impl Input {
    /// The binding token for a key event, as written in configuration.
    /// Shift is only spelled out for named keys; a shifted character is itself.
    pub fn token(&self) -> Option<String> {
        let Input::Key {
            key,
            ctrl,
            alt,
            shift,
        } = self
        else {
            return None;
        };
        Some(format!(
            "{}{}{}{}",
            if *ctrl { "ctrl-" } else { "" },
            if *alt { "alt-" } else { "" },
            if *shift && key.chars().count() != 1 {
                "shift-"
            } else {
                ""
            },
            key
        ))
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Frame {
    pub paint: String,
    pub detach: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::*;

    fn key(key: &str, ctrl: bool, alt: bool, shift: bool) -> Input {
        Input::Key {
            key: key.into(),
            ctrl,
            alt,
            shift,
        }
    }

    #[test]
    fn binding_tokens_match_configuration_spelling() -> crate::testing::Outcome {
        assert_eq!(key("b", false, false, false).token().need()?, "b");
        assert_eq!(key("b", true, false, false).token().need()?, "ctrl-b");
        assert_eq!(key("left", false, true, false).token().need()?, "alt-left");
        assert_eq!(key("tab", false, false, true).token().need()?, "shift-tab");
        assert_eq!(key("T", false, false, true).token().need()?, "T");
        assert_eq!(
            key("up", true, true, true).token().need()?,
            "ctrl-alt-shift-up"
        );
        assert!(Input::PasteBegin.token().is_none());
        assert!(Input::Resize { rows: 1, cols: 1 }.token().is_none());
        Ok(())
    }
}
