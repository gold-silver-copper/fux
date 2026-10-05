//! Mouse events and their reports, as xterm sends them
//! (`references/xterm/ctlseqs.html`, "Mouse Tracking"): what a user's
//! terminal reports decodes to a [`MouseEvent`] ([`crate::keys::decode`]),
//! and [`crate::Screen::encode_mouse`] reports one to a program in the mode
//! and encoding it asked for.
//!
//! A report's button code is the button (0 left, 1 middle, 2 right, 3 a
//! release in the encodings that do not name the button; 64 to 67 the
//! wheel; 128 and 129 the back and forward buttons), plus 4 for Shift, 8 for
//! Alt and 16 for Ctrl, plus 32 for motion. Positions count from one.
use crate::keys::Modifiers;
use crate::{MouseProtocolEncoding, MouseProtocolMode};
use std::io::Write;

/// A mouse button.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MouseButton {
    /// The left button (button 1).
    Left,
    /// The middle button (button 2).
    Middle,
    /// The right button (button 3).
    Right,
    /// The wheel turned up (button 4).
    WheelUp,
    /// The wheel turned down (button 5).
    WheelDown,
    /// The wheel tilted left (button 6).
    WheelLeft,
    /// The wheel tilted right (button 7).
    WheelRight,
    /// The back button (button 8).
    Back,
    /// The forward button (button 9).
    Forward,
}

impl MouseButton {
    /// Whether this is the wheel, whose turns are presses never released.
    pub fn is_wheel(self) -> bool {
        matches!(
            self,
            MouseButton::WheelUp
                | MouseButton::WheelDown
                | MouseButton::WheelLeft
                | MouseButton::WheelRight
        )
    }

    /// The button's part of a report's button code.
    fn code(self) -> u32 {
        match self {
            MouseButton::Left => 0,
            MouseButton::Middle => 1,
            MouseButton::Right => 2,
            MouseButton::WheelUp => 64,
            MouseButton::WheelDown => 65,
            MouseButton::WheelLeft => 66,
            MouseButton::WheelRight => 67,
            MouseButton::Back => 128,
            MouseButton::Forward => 129,
        }
    }

    /// The button a report's button code names, its modifier and motion
    /// bits aside; `None` for 3 (a release or motion with no button named)
    /// and for codes no button has.
    fn from_code(code: u32) -> Option<MouseButton> {
        Some(match code & !(SHIFT | ALT | CTRL | MOTION) {
            0 => MouseButton::Left,
            1 => MouseButton::Middle,
            2 => MouseButton::Right,
            64 => MouseButton::WheelUp,
            65 => MouseButton::WheelDown,
            66 => MouseButton::WheelLeft,
            67 => MouseButton::WheelRight,
            128 => MouseButton::Back,
            129 => MouseButton::Forward,
            _ => return None,
        })
    }
}

/// What a mouse event is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MouseAction {
    /// A button pressed, or the wheel turned.
    Press,
    /// A button released.
    Release,
    /// The pointer moved to another cell.
    Motion,
}

/// A mouse event over a screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MouseEvent {
    /// What happened.
    pub action: MouseAction,
    /// The button pressed or released, or held during motion; `None` for
    /// motion with no button held, and for a release whose terminal did not
    /// say which button (the default and UTF-8 encodings never do).
    pub button: Option<MouseButton>,
    /// The modifiers held.
    pub mods: Modifiers,
    /// The cell's row, from zero at the top of the screen.
    pub row: u16,
    /// The cell's column, from zero at the left.
    pub col: u16,
}

/// The modifier and motion bits of a report's button code.
const SHIFT: u32 = 4;
const ALT: u32 = 8;
const CTRL: u32 = 16;
const MOTION: u32 = 32;
/// The button code of a release in the encodings that do not name the
/// button, and of motion with no button held.
const NO_BUTTON: u32 = 3;
/// The largest position the default encoding carries: 32 more is the
/// largest byte.
const DEFAULT_LIMIT: u32 = 223;
/// The largest position the UTF-8 encoding carries, as xterm has it: 32
/// more is the largest two-byte character.
const UTF8_LIMIT: u32 = 2015;

impl crate::Screen {
    /// Appends `event`'s report to `out` as the program on this screen asked
    /// for mouse reports (`CSI ? 9 / 1000 / 1002 / 1003 h`, encoded as
    /// `CSI ? 1005 / 1006 h` say), and returns whether there was one
    /// ([`mouse_bytes`]).
    pub fn encode_mouse(&self, event: MouseEvent, out: &mut Vec<u8>) -> bool {
        mouse_bytes(
            event,
            self.mouse_protocol_mode(),
            self.mouse_protocol_encoding(),
            out,
        )
    }
}

/// Appends `event`'s report to `out` in `mode` and `encoding`, as xterm
/// reports it, and returns whether there was one:
/// - with no mode, nothing is reported;
/// - X10 (`Press`, mode 9) reports presses of the left, middle and right
///   buttons alone, without modifiers;
/// - `PressRelease` (1000) reports presses and releases, the wheel's turns
///   as presses and never released;
/// - `ButtonMotion` (1002) also reports motion while a button is held;
/// - `AnyMotion` (1003) also reports motion with none held;
/// - a press names its button; motion over a cell is the host's to tell
///   from motion within it (xterm reports a move to another cell only);
/// - the default encoding sends `CSI M` and three bytes, each value plus
///   32, and nothing for a position past 223; UTF-8 (1005) sends each value
///   plus 32 as a UTF-8 character, the button code too, and nothing past
///   2015; SGR (1006) sends
///   `CSI < code ; column ; row M`, or `m` for a release, which names its
///   button, for any position.
pub fn mouse_bytes(
    event: MouseEvent,
    mode: MouseProtocolMode,
    encoding: MouseProtocolEncoding,
    out: &mut Vec<u8>,
) -> bool {
    let wheel = event.button.is_some_and(MouseButton::is_wheel);
    let reported = match (mode, event.action) {
        (MouseProtocolMode::None, _) => false,
        // X10 knew three buttons: "Cb is button−1, where button is 1, 2 or 3".
        (MouseProtocolMode::Press, MouseAction::Press) => matches!(
            event.button,
            Some(MouseButton::Left | MouseButton::Middle | MouseButton::Right)
        ),
        (_, MouseAction::Press) => event.button.is_some(),
        (MouseProtocolMode::Press, _) => false,
        (_, MouseAction::Release) => !wheel,
        (MouseProtocolMode::PressRelease, MouseAction::Motion) => false,
        (MouseProtocolMode::ButtonMotion, MouseAction::Motion) => event.button.is_some() && !wheel,
        (MouseProtocolMode::AnyMotion, MouseAction::Motion) => !wheel,
    };
    if !reported {
        return false;
    }
    let names_release = encoding == MouseProtocolEncoding::Sgr;
    let mut code = match event.button {
        Some(button) if event.action != MouseAction::Release || names_release => button.code(),
        _ => NO_BUTTON,
    };
    if mode != MouseProtocolMode::Press {
        let mods = event.mods;
        for (held, bit) in [(mods.shift, SHIFT), (mods.alt, ALT), (mods.ctrl, CTRL)] {
            if held {
                code |= bit;
            }
        }
    }
    if event.action == MouseAction::Motion {
        code |= MOTION;
    }
    let (x, y) = (
        u32::from(event.col).saturating_add(1),
        u32::from(event.row).saturating_add(1),
    );
    match encoding {
        MouseProtocolEncoding::Sgr => {
            let last = if event.action == MouseAction::Release {
                'm'
            } else {
                'M'
            };
            // Writing to a Vec cannot fail.
            let _ = write!(out, "\x1b[<{code};{x};{y}{last}");
        }
        MouseProtocolEncoding::Default => {
            if x > DEFAULT_LIMIT || y > DEFAULT_LIMIT {
                return false;
            }
            let byte = |n: u32| u8::try_from(n.saturating_add(32)).unwrap_or(u8::MAX);
            out.extend_from_slice(b"\x1b[M");
            out.extend_from_slice(&[byte(code), byte(x), byte(y)]);
        }
        MouseProtocolEncoding::Utf8 => {
            if x > UTF8_LIMIT || y > UTF8_LIMIT {
                return false;
            }
            out.extend_from_slice(b"\x1b[M");
            for n in [code, x, y] {
                let c = char::from_u32(n.saturating_add(32)).unwrap_or(char::MAX);
                let mut utf8 = [0; 4];
                out.extend_from_slice(c.encode_utf8(&mut utf8).as_bytes());
            }
        }
    }
    true
}

/// The event a report's button code, column and row (from one) and its
/// form describe: `sgr_release` for SGR's `m`; `None` for a code no button
/// has, a position of zero or past the screen API's reach.
pub(crate) fn event(code: u32, x: u32, y: u32, sgr_release: bool) -> Option<MouseEvent> {
    let col = u16::try_from(x.checked_sub(1)?).ok()?;
    let row = u16::try_from(y.checked_sub(1)?).ok()?;
    let button = MouseButton::from_code(code);
    let unnamed = code & !(SHIFT | ALT | CTRL | MOTION) == NO_BUTTON;
    if button.is_none() && !unnamed {
        return None;
    }
    let action = if code & MOTION != 0 {
        MouseAction::Motion
    } else if sgr_release || unnamed {
        MouseAction::Release
    } else {
        MouseAction::Press
    };
    Some(MouseEvent {
        action,
        button,
        mods: Modifiers {
            ctrl: code & CTRL != 0,
            alt: code & ALT != 0,
            shift: code & SHIFT != 0,
        },
        row,
        col,
    })
}

#[cfg(test)]
mod tests;
