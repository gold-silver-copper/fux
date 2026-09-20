use bevy_ecs::prelude::*;
use bevy_math::{URect, UVec2};
use bevy_reflect::{Reflect, ReflectDeserialize, ReflectSerialize};
use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PaneRect {
    /// IDs in the authoritative server World, not the presentation World.
    pub leaf: Entity,
    pub pane: Entity,
    /// Content cells, half-open: `min` is the first cell, `max` one past the
    /// last. No border or title inset; the final viewer row is chrome.
    pub rect: URect,
}

impl PaneRect {
    pub fn x(&self) -> u16 {
        self.rect.min.x as u16
    }
    pub fn y(&self) -> u16 {
        self.rect.min.y as u16
    }
    pub fn width(&self) -> u16 {
        self.rect.width() as u16
    }
    pub fn height(&self) -> u16 {
        self.rect.height() as u16
    }
    /// Half-open containment; `URect::contains` includes the far edge.
    pub fn covers(&self, x: u16, y: u16) -> bool {
        let point = UVec2::new(u32::from(x), u32::from(y));
        point.cmpge(self.rect.min).all() && point.cmplt(self.rect.max).all()
    }
    /// The cell inside this pane, zero-based `(row, column)`, clamped to it.
    pub fn local(&self, x: u16, y: u16) -> (u16, u16) {
        (
            y.saturating_sub(self.y())
                .min(self.height().saturating_sub(1)),
            x.saturating_sub(self.x())
                .min(self.width().saturating_sub(1)),
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Reflect, Serialize, Deserialize)]
#[reflect(Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    Left,
    Right,
    Up,
    Down,
}

/// A key as the outer terminal names it. The wire form is the configuration
/// spelling: one character for `Char`, `enter`, `up`, `f5` and so on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Reflect, Serialize, Deserialize)]
#[reflect(Serialize, Deserialize)]
#[serde(into = "String", try_from = "String")]
pub enum Key {
    Char(char),
    Enter,
    Tab,
    Escape,
    Backspace,
    Delete,
    Insert,
    Arrow(Direction),
    Home,
    End,
    PageUp,
    PageDown,
    /// Only `1..=12` deserializes; `convert` never produces others.
    F(u8),
}

impl fmt::Display for Key {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Char(c) => write!(f, "{c}"),
            Self::Enter => f.write_str("enter"),
            Self::Tab => f.write_str("tab"),
            Self::Escape => f.write_str("escape"),
            Self::Backspace => f.write_str("backspace"),
            Self::Delete => f.write_str("delete"),
            Self::Insert => f.write_str("insert"),
            Self::Arrow(Direction::Left) => f.write_str("left"),
            Self::Arrow(Direction::Right) => f.write_str("right"),
            Self::Arrow(Direction::Up) => f.write_str("up"),
            Self::Arrow(Direction::Down) => f.write_str("down"),
            Self::Home => f.write_str("home"),
            Self::End => f.write_str("end"),
            Self::PageUp => f.write_str("pageup"),
            Self::PageDown => f.write_str("pagedown"),
            Self::F(n) => write!(f, "f{n}"),
        }
    }
}
impl From<Key> for String {
    fn from(key: Key) -> String {
        key.to_string()
    }
}
impl std::str::FromStr for Key {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        let mut chars = s.chars();
        if let (Some(c), None) = (chars.next(), chars.next()) {
            return Ok(Self::Char(c));
        }
        Ok(match s {
            "enter" => Self::Enter,
            "tab" => Self::Tab,
            "escape" => Self::Escape,
            "backspace" => Self::Backspace,
            "delete" => Self::Delete,
            "insert" => Self::Insert,
            "left" => Self::Arrow(Direction::Left),
            "right" => Self::Arrow(Direction::Right),
            "up" => Self::Arrow(Direction::Up),
            "down" => Self::Arrow(Direction::Down),
            "home" => Self::Home,
            "end" => Self::End,
            "pageup" => Self::PageUp,
            "pagedown" => Self::PageDown,
            _ => {
                let number = s.strip_prefix('f').and_then(|n| n.parse::<u8>().ok());
                match number {
                    Some(n) if (1..=12).contains(&n) => Self::F(n),
                    _ => return Err(format!("unsupported key {s}")),
                }
            }
        })
    }
}
impl TryFrom<String> for Key {
    type Error = String;
    fn try_from(s: String) -> Result<Self, String> {
        s.parse()
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Reflect, Serialize, Deserialize)]
#[reflect(Serialize, Deserialize)]
pub struct Modifiers {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Reflect, Serialize, Deserialize)]
#[reflect(Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MouseAction {
    Press,
    Release,
    Move,
    ScrollUp,
    ScrollDown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Reflect, Serialize, Deserialize)]
#[reflect(Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MouseButton {
    Left,
    Middle,
    Right,
    None,
}

#[derive(Debug, Clone, Serialize, Deserialize, Reflect)]
#[reflect(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Input {
    Key {
        key: Key,
        #[serde(flatten)]
        modifiers: Modifiers,
    },
    /// Capture the current input owner before a fragmented bracketed paste.
    PasteBegin,
    Paste {
        text: String,
    },
    Mouse {
        action: MouseAction,
        button: MouseButton,
        x: u16,
        y: u16,
        #[serde(flatten)]
        modifiers: Modifiers,
    },
    Resize {
        rows: u16,
        cols: u16,
    },
}

/// A binding as written in configuration: `ctrl-alt-shift-key`. Shift is only
/// spelled out for named keys; a shifted character is itself.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Reflect, Serialize, Deserialize)]
#[reflect(Serialize, Deserialize)]
#[serde(transparent)]
pub struct Token(String);

impl Token {
    pub fn new(key: Key, modifiers: Modifiers) -> Self {
        Self(format!(
            "{}{}{}{key}",
            if modifiers.ctrl { "ctrl-" } else { "" },
            if modifiers.alt { "alt-" } else { "" },
            if modifiers.shift && !matches!(key, Key::Char(_)) {
                "shift-"
            } else {
                ""
            },
        ))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}
impl From<&str> for Token {
    fn from(text: &str) -> Self {
        Self(text.to_owned())
    }
}
impl fmt::Display for Token {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl Input {
    /// The binding token for a key event, as written in configuration.
    pub fn token(&self) -> Option<Token> {
        match self {
            Input::Key { key, modifiers } => Some(Token::new(*key, *modifiers)),
            _ => None,
        }
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

    fn key(key: &str, ctrl: bool, alt: bool, shift: bool) -> Result<Input, String> {
        Ok(Input::Key {
            key: key.parse()?,
            modifiers: Modifiers { ctrl, alt, shift },
        })
    }

    #[test]
    fn binding_tokens_match_configuration_spelling() -> crate::testing::Outcome {
        assert_eq!(key("b", false, false, false)?.token().need()?.as_str(), "b");
        assert_eq!(
            key("b", true, false, false)?.token().need()?.as_str(),
            "ctrl-b"
        );
        assert_eq!(
            key("left", false, true, false)?.token().need()?.as_str(),
            "alt-left"
        );
        assert_eq!(
            key("tab", false, false, true)?.token().need()?.as_str(),
            "shift-tab"
        );
        assert_eq!(key("T", false, false, true)?.token().need()?.as_str(), "T");
        assert_eq!(
            key("up", true, true, true)?.token().need()?.as_str(),
            "ctrl-alt-shift-up"
        );
        assert!(Input::PasteBegin.token().is_none());
        assert!(Input::Resize { rows: 1, cols: 1 }.token().is_none());
        Ok(())
    }

    #[test]
    fn every_key_round_trips_and_bad_names_are_rejected() -> crate::testing::Outcome {
        let mut keys = vec![
            Key::Char('a'),
            Key::Char('界'),
            Key::Enter,
            Key::Tab,
            Key::Escape,
            Key::Backspace,
            Key::Delete,
            Key::Insert,
            Key::Home,
            Key::End,
            Key::PageUp,
            Key::PageDown,
        ];
        keys.extend(
            [
                Direction::Left,
                Direction::Right,
                Direction::Up,
                Direction::Down,
            ]
            .map(Key::Arrow),
        );
        keys.extend((1..=12).map(Key::F));
        for key in keys {
            let json = serde_json::to_string(&key)?;
            assert_eq!(serde_json::from_str::<Key>(&json)?, key);
            assert_eq!(key.to_string().parse::<Key>()?, key);
        }
        assert_eq!(serde_json::to_string(&Key::F(5))?, "\"f5\"");
        for bad in ["f0", "f13", "", "fno", "unknown"] {
            assert!(bad.parse::<Key>().is_err(), "{bad}");
        }
        let input: Input = serde_json::from_str(
            r#"{"kind":"key","key":"enter","ctrl":true,"alt":false,"shift":false}"#,
        )?;
        assert!(matches!(
            input,
            Input::Key {
                key: Key::Enter,
                modifiers: Modifiers {
                    ctrl: true,
                    alt: false,
                    shift: false
                }
            }
        ));
        Ok(())
    }
}
