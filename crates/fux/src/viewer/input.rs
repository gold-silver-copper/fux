//! Terminal input parsed once into `bevy_input` messages (prompt 3.11): termina events become
//! `KeyboardInput` (with the terminal's own bytes in `text`), `MouseButtonInput`/`MouseMotion`/
//! `MouseWheel` plus a `PointerInput` for the local `PointerId::Mouse`, and `Ime::Commit` for
//! bracketed paste. Resize and focus events are reported to the caller as plain values.

use bevy_camera::NormalizedRenderTarget;
use bevy_ecs::prelude::*;
use bevy_input::ButtonState;
use bevy_input::keyboard::{Key, KeyCode, KeyboardInput, NativeKey, NativeKeyCode};
use bevy_input::mouse::{MouseButton, MouseButtonInput, MouseMotion, MouseScrollUnit, MouseWheel};
use bevy_input::touch::TouchPhase;
use bevy_math::Vec2;
use bevy_picking::pointer::{Location, PointerAction, PointerButton, PointerId, PointerInput};
use bevy_window::Ime;
use termina::event::{
    Event, KeyCode as TKey, KeyEvent, KeyEventKind, Modifiers, MouseButton as TButton, MouseEvent,
    MouseEventKind,
};

use crate::model::{PointerEvent, PointerKind};

/// Modifier bits carried in `PointerEvent::modifiers` (xterm order: shift, alt, ctrl).
pub const MOD_SHIFT: u8 = 1;
pub const MOD_ALT: u8 = 2;
pub const MOD_CTRL: u8 = 4;

/// One terminal event after translation.
#[derive(Debug, Clone)]
pub enum Input {
    /// A key press; the caller also emits the matching release so `ButtonInput` never sticks.
    Key(KeyboardInput),
    Mouse(MouseInput),
    Paste(Ime),
    Resize {
        cols: u16,
        rows: u16,
    },
    Focus(bool),
}

/// A mouse event fanned out to every consumer: `bevy_input` messages, the picking pointer, and
/// the server-bound `PointerEvent`.
#[derive(Debug, Clone)]
pub struct MouseInput {
    pub button: Option<MouseButtonInput>,
    pub motion: Option<MouseMotion>,
    pub wheel: Option<MouseWheel>,
    /// A pointer move to the event's cell, when it differs from the previous one.
    pub pointer_move: Option<PointerInput>,
    /// The press/release/scroll action itself.
    pub pointer_action: Option<PointerInput>,
    pub event: PointerEvent,
}

/// State the translator carries between events: the target window and pointer, the viewport
/// (which names the pointer's render target) and the last pointer cell for motion deltas.
#[derive(Debug, Clone)]
pub struct Translator {
    pub window: Entity,
    pub cols: u16,
    pub rows: u16,
    last_mouse: Option<Vec2>,
}

impl Translator {
    pub fn new(window: Entity, cols: u16, rows: u16) -> Self {
        Self {
            window,
            cols,
            rows,
            last_mouse: None,
        }
    }

    /// Translates one termina event. `None` for events with no `bevy_input` meaning (key
    /// releases, terminal responses, media keys).
    pub fn translate(&mut self, event: &Event) -> Option<Input> {
        match event {
            Event::Key(key) => translate_key(key, self.window).map(Input::Key),
            Event::Mouse(mouse) => Some(Input::Mouse(self.translate_mouse(mouse))),
            Event::Paste(text) => Some(Input::Paste(Ime::Commit {
                window: self.window,
                value: text.clone(),
            })),
            Event::WindowResized(size) => {
                self.cols = size.cols;
                self.rows = size.rows;
                Some(Input::Resize {
                    cols: size.cols,
                    rows: size.rows,
                })
            }
            Event::FocusIn => Some(Input::Focus(true)),
            Event::FocusOut => Some(Input::Focus(false)),
            Event::Csi(_) | Event::Osc(_) | Event::Dcs(_) => None,
        }
    }

    fn location(&self, col: u16, row: u16) -> Location {
        Location {
            target: NormalizedRenderTarget::None {
                width: u32::from(self.cols),
                height: u32::from(self.rows),
            },
            // Cell centres, so a hit test against whole-cell node rects is unambiguous.
            position: Vec2::new(f32::from(col) + 0.5, f32::from(row) + 0.5),
        }
    }

    fn translate_mouse(&mut self, mouse: &MouseEvent) -> MouseInput {
        let location = self.location(mouse.column, mouse.row);
        let motion = match self.last_mouse {
            Some(last) if last != location.position => Some(MouseMotion {
                delta: location.position - last,
            }),
            None => Some(MouseMotion { delta: Vec2::ZERO }),
            Some(_) => None,
        };
        let pointer_move = motion.map(|m| {
            PointerInput::new(
                PointerId::Mouse,
                location.clone(),
                PointerAction::Move { delta: m.delta },
            )
        });
        self.last_mouse = Some(location.position);
        let modifiers = modifier_bits(mouse.modifiers);
        let (button, wheel, pointer_action, kind, ev_button) = match mouse.kind {
            MouseEventKind::Down(b) => (
                Some(MouseButtonInput {
                    button: mouse_button(b),
                    state: ButtonState::Pressed,
                    window: self.window,
                }),
                None,
                Some(PointerAction::Press(pointer_button(b))),
                PointerKind::Press,
                model_button(b),
            ),
            MouseEventKind::Up(b) => (
                Some(MouseButtonInput {
                    button: mouse_button(b),
                    state: ButtonState::Released,
                    window: self.window,
                }),
                None,
                Some(PointerAction::Release(pointer_button(b))),
                PointerKind::Release,
                model_button(b),
            ),
            MouseEventKind::Drag(b) => (None, None, None, PointerKind::Move, model_button(b)),
            MouseEventKind::Moved => (
                None,
                None,
                None,
                PointerKind::Move,
                crate::model::PointerButton::None,
            ),
            MouseEventKind::ScrollUp
            | MouseEventKind::ScrollDown
            | MouseEventKind::ScrollLeft
            | MouseEventKind::ScrollRight => {
                let (x, y) = match mouse.kind {
                    MouseEventKind::ScrollUp => (0.0, 1.0),
                    MouseEventKind::ScrollDown => (0.0, -1.0),
                    MouseEventKind::ScrollLeft => (-1.0, 0.0),
                    _ => (1.0, 0.0),
                };
                let kind = match mouse.kind {
                    MouseEventKind::ScrollUp => PointerKind::ScrollUp,
                    MouseEventKind::ScrollDown => PointerKind::ScrollDown,
                    _ => PointerKind::Move,
                };
                (
                    None,
                    Some(MouseWheel {
                        unit: MouseScrollUnit::Line,
                        x,
                        y,
                        window: self.window,
                        phase: TouchPhase::Moved,
                    }),
                    Some(PointerAction::Scroll {
                        unit: MouseScrollUnit::Line,
                        x,
                        y,
                        phase: TouchPhase::Moved,
                    }),
                    kind,
                    crate::model::PointerButton::None,
                )
            }
        };
        let pointer_action =
            pointer_action.map(|action| PointerInput::new(PointerId::Mouse, location, action));
        MouseInput {
            button,
            motion,
            wheel,
            pointer_move,
            pointer_action,
            event: PointerEvent {
                col: mouse.column,
                row: mouse.row,
                kind,
                button: ev_button,
                modifiers,
            },
        }
    }
}

fn modifier_bits(m: Modifiers) -> u8 {
    let mut bits = 0;
    if m.contains(Modifiers::SHIFT) {
        bits |= MOD_SHIFT;
    }
    if m.contains(Modifiers::ALT) {
        bits |= MOD_ALT;
    }
    if m.contains(Modifiers::CONTROL) {
        bits |= MOD_CTRL;
    }
    bits
}

fn mouse_button(b: TButton) -> MouseButton {
    match b {
        TButton::Left => MouseButton::Left,
        TButton::Right => MouseButton::Right,
        TButton::Middle => MouseButton::Middle,
    }
}

fn pointer_button(b: TButton) -> PointerButton {
    match b {
        TButton::Left => PointerButton::Primary,
        TButton::Right => PointerButton::Secondary,
        TButton::Middle => PointerButton::Middle,
    }
}

fn model_button(b: TButton) -> crate::model::PointerButton {
    match b {
        TButton::Left => crate::model::PointerButton::Left,
        TButton::Right => crate::model::PointerButton::Right,
        TButton::Middle => crate::model::PointerButton::Middle,
    }
}

/// Translates a key press into a `KeyboardInput` whose `text` holds the bytes the terminal
/// would send a program for that key (normal cursor mode). Releases yield `None`.
pub fn translate_key(key: &KeyEvent, window: Entity) -> Option<KeyboardInput> {
    if key.kind == KeyEventKind::Release {
        return None;
    }
    let shift = key.modifiers.contains(Modifiers::SHIFT);
    let alt = key.modifiers.contains(Modifiers::ALT);
    let ctrl = key.modifiers.contains(Modifiers::CONTROL);
    let param = 1 + u8::from(shift) + 2 * u8::from(alt) + 4 * u8::from(ctrl);
    let mut text = String::new();
    let (key_code, logical) = match key.code {
        TKey::Char(c) => {
            if alt {
                text.push('\x1b');
            }
            match control_byte(c, ctrl) {
                Some(byte) => text.push(char::from(byte)),
                None => text.push(c),
            }
            let mut buf = [0u8; 4];
            (
                key_code_for_char(c),
                Key::Character((*c.encode_utf8(&mut buf)).into()),
            )
        }
        TKey::Enter => (
            KeyCode::Enter,
            simple(&mut text, alt, if ctrl { "\n" } else { "\r" }, Key::Enter),
        ),
        TKey::Tab => (KeyCode::Tab, simple(&mut text, alt, "\t", Key::Tab)),
        TKey::BackTab => (KeyCode::Tab, simple(&mut text, false, "\x1b[Z", Key::Tab)),
        TKey::Backspace => (
            KeyCode::Backspace,
            simple(
                &mut text,
                alt,
                if ctrl { "\x08" } else { "\x7f" },
                Key::Backspace,
            ),
        ),
        TKey::Escape => (KeyCode::Escape, simple(&mut text, alt, "\x1b", Key::Escape)),
        TKey::Up => (
            KeyCode::ArrowUp,
            csi_letter(&mut text, param, 'A', Key::ArrowUp),
        ),
        TKey::Down => (
            KeyCode::ArrowDown,
            csi_letter(&mut text, param, 'B', Key::ArrowDown),
        ),
        TKey::Right => (
            KeyCode::ArrowRight,
            csi_letter(&mut text, param, 'C', Key::ArrowRight),
        ),
        TKey::Left => (
            KeyCode::ArrowLeft,
            csi_letter(&mut text, param, 'D', Key::ArrowLeft),
        ),
        TKey::Home => (KeyCode::Home, csi_letter(&mut text, param, 'H', Key::Home)),
        TKey::End => (KeyCode::End, csi_letter(&mut text, param, 'F', Key::End)),
        TKey::Insert => (KeyCode::Insert, csi_tilde(&mut text, param, 2, Key::Insert)),
        TKey::Delete => (KeyCode::Delete, csi_tilde(&mut text, param, 3, Key::Delete)),
        TKey::PageUp => (KeyCode::PageUp, csi_tilde(&mut text, param, 5, Key::PageUp)),
        TKey::PageDown => (
            KeyCode::PageDown,
            csi_tilde(&mut text, param, 6, Key::PageDown),
        ),
        TKey::Function(n) => function_key(&mut text, param, n)?,
        TKey::KeypadBegin
        | TKey::CapsLock
        | TKey::ScrollLock
        | TKey::NumLock
        | TKey::PrintScreen
        | TKey::Pause
        | TKey::Menu
        | TKey::Null
        | TKey::Modifier(_)
        | TKey::Media(_) => return None,
    };
    Some(KeyboardInput {
        key_code,
        logical_key: logical,
        state: ButtonState::Pressed,
        text: Some(text.into()),
        repeat: key.kind == KeyEventKind::Repeat,
        window,
    })
}

fn simple(text: &mut String, alt: bool, bytes: &str, key: Key) -> Key {
    if alt {
        text.push('\x1b');
    }
    text.push_str(bytes);
    key
}

fn csi_letter(text: &mut String, param: u8, letter: char, key: Key) -> Key {
    text.push_str("\x1b[");
    if param > 1 {
        text.push_str("1;");
        push_num(text, param);
    }
    text.push(letter);
    key
}

fn csi_tilde(text: &mut String, param: u8, code: u8, key: Key) -> Key {
    text.push_str("\x1b[");
    push_num(text, code);
    if param > 1 {
        text.push(';');
        push_num(text, param);
    }
    text.push('~');
    key
}

fn push_num(text: &mut String, n: u8) {
    use core::fmt::Write as _;
    // `String::write_fmt` cannot fail.
    let _ = write!(text, "{n}");
}

fn function_key(text: &mut String, param: u8, n: u8) -> Option<(KeyCode, Key)> {
    let (code, key) = match n {
        1 => (KeyCode::F1, Key::F1),
        2 => (KeyCode::F2, Key::F2),
        3 => (KeyCode::F3, Key::F3),
        4 => (KeyCode::F4, Key::F4),
        5 => (KeyCode::F5, Key::F5),
        6 => (KeyCode::F6, Key::F6),
        7 => (KeyCode::F7, Key::F7),
        8 => (KeyCode::F8, Key::F8),
        9 => (KeyCode::F9, Key::F9),
        10 => (KeyCode::F10, Key::F10),
        11 => (KeyCode::F11, Key::F11),
        12 => (KeyCode::F12, Key::F12),
        _ => return None,
    };
    match n {
        1..=4 => {
            let letter = char::from(b'P' + (n - 1));
            if param > 1 {
                text.push_str("\x1b[1;");
                push_num(text, param);
            } else {
                text.push_str("\x1bO");
            }
            text.push(letter);
        }
        _ => {
            let code = match n {
                5 => 15,
                6 => 17,
                7 => 18,
                8 => 19,
                9 => 20,
                10 => 21,
                11 => 23,
                _ => 24,
            };
            csi_tilde(
                text,
                param,
                code,
                Key::Unidentified(NativeKey::Unidentified),
            );
        }
    }
    Some((code, key))
}

fn control_byte(c: char, ctrl: bool) -> Option<u8> {
    if !ctrl {
        return None;
    }
    match c {
        'a'..='z' => Some(c as u8 & 0x1f),
        'A'..='Z' => Some(c.to_ascii_lowercase() as u8 & 0x1f),
        ' ' | '@' | '2' => Some(0),
        '[' | '3' => Some(0x1b),
        '\\' | '4' => Some(0x1c),
        ']' | '5' => Some(0x1d),
        '^' | '6' => Some(0x1e),
        '_' | '7' | '-' => Some(0x1f),
        '?' | '8' => Some(0x7f),
        _ => None,
    }
}

fn key_code_for_char(c: char) -> KeyCode {
    match c.to_ascii_lowercase() {
        'a' => KeyCode::KeyA,
        'b' => KeyCode::KeyB,
        'c' => KeyCode::KeyC,
        'd' => KeyCode::KeyD,
        'e' => KeyCode::KeyE,
        'f' => KeyCode::KeyF,
        'g' => KeyCode::KeyG,
        'h' => KeyCode::KeyH,
        'i' => KeyCode::KeyI,
        'j' => KeyCode::KeyJ,
        'k' => KeyCode::KeyK,
        'l' => KeyCode::KeyL,
        'm' => KeyCode::KeyM,
        'n' => KeyCode::KeyN,
        'o' => KeyCode::KeyO,
        'p' => KeyCode::KeyP,
        'q' => KeyCode::KeyQ,
        'r' => KeyCode::KeyR,
        's' => KeyCode::KeyS,
        't' => KeyCode::KeyT,
        'u' => KeyCode::KeyU,
        'v' => KeyCode::KeyV,
        'w' => KeyCode::KeyW,
        'x' => KeyCode::KeyX,
        'y' => KeyCode::KeyY,
        'z' => KeyCode::KeyZ,
        '0' => KeyCode::Digit0,
        '1' => KeyCode::Digit1,
        '2' => KeyCode::Digit2,
        '3' => KeyCode::Digit3,
        '4' => KeyCode::Digit4,
        '5' => KeyCode::Digit5,
        '6' => KeyCode::Digit6,
        '7' => KeyCode::Digit7,
        '8' => KeyCode::Digit8,
        '9' => KeyCode::Digit9,
        ' ' => KeyCode::Space,
        '-' => KeyCode::Minus,
        '=' => KeyCode::Equal,
        '[' => KeyCode::BracketLeft,
        ']' => KeyCode::BracketRight,
        '\\' => KeyCode::Backslash,
        ';' => KeyCode::Semicolon,
        '\'' => KeyCode::Quote,
        ',' => KeyCode::Comma,
        '.' => KeyCode::Period,
        '/' => KeyCode::Slash,
        '`' => KeyCode::Backquote,
        _ => KeyCode::Unidentified(NativeKeyCode::Unidentified),
    }
}
