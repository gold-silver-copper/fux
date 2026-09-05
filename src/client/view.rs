//! Render-facing workspace interfaces and visual overlays; no prediction engine or transport.
//! Terminal mode emission adapted from koh under MIT; see LICENSES/koh.txt.
use std::collections::BTreeMap;
use vt100::Color;

#[derive(Clone, Debug)]
pub struct OverlayCell {
    pub glyph: String,
    pub fg: Color,
    pub bg: Color,
    pub underline: bool,
    pub unknown: bool,
}
#[derive(Default, Debug)]
pub struct Overlay {
    pub cells: BTreeMap<(u16, u16), OverlayCell>,
    pub cursor: Option<(u16, u16)>,
}
impl Overlay {
    pub fn empty() -> Self {
        Self::default()
    }
    pub fn cell(&self, row: u16, column: u16) -> Option<&OverlayCell> {
        self.cells.get(&(row, column))
    }
    pub fn cursor(&self) -> Option<(u16, u16)> {
        self.cursor
    }
    pub fn is_empty(&self) -> bool {
        self.cells.is_empty() && self.cursor.is_none()
    }
}
pub struct WindowState<'a> {
    pub title: &'a str,
    pub icon: &'a str,
    pub clipboard: &'a str,
    pub bell_count: u64,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct InputModes {
    pub application_keypad: bool,
    pub application_cursor: bool,
    pub bracketed_paste: bool,
    pub mouse_mode: vt100::MouseProtocolMode,
    pub mouse_encoding: vt100::MouseProtocolEncoding,
}
pub trait ClientState: Send + 'static {
    fn window(&self) -> WindowState<'_>;
    fn exit_code(&self) -> Option<u32>;
    fn input_modes(&self) -> InputModes {
        InputModes::default()
    }
}
pub trait ClientTerminal<S: ClientState> {
    fn render(&mut self, state: &S, overlay: &Overlay, status: Option<&str>)
    -> std::io::Result<()>;
    fn size(&self) -> std::io::Result<(u16, u16)>;
    fn suspend_resume(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl InputModes {
    /// Escape sequences setting every mode explicitly (the first frame / after a resume).
    pub fn formatted(self) -> Vec<u8> {
        self.write(&mut Vec::new(), None)
    }

    /// Escape sequences taking a terminal at `prev` to these modes (only the changes).
    pub fn diff(self, prev: Self) -> Vec<u8> {
        self.write(&mut Vec::new(), Some(prev))
    }

    fn write(self, buf: &mut Vec<u8>, prev: Option<Self>) -> Vec<u8> {
        use vt100::{MouseProtocolEncoding as Enc, MouseProtocolMode as Mode};
        let changed = |get: fn(&Self) -> bool| prev.is_none_or(|p| get(&p) != get(&self));
        if changed(|m| m.application_keypad) {
            buf.extend_from_slice(if self.application_keypad {
                b"\x1b="
            } else {
                b"\x1b>"
            });
        }
        if changed(|m| m.application_cursor) {
            buf.extend_from_slice(if self.application_cursor {
                b"\x1b[?1h"
            } else {
                b"\x1b[?1l"
            });
        }
        if changed(|m| m.bracketed_paste) {
            buf.extend_from_slice(if self.bracketed_paste {
                b"\x1b[?2004h"
            } else {
                b"\x1b[?2004l"
            });
        }
        let prev_mode = prev.map_or(Mode::None, |p| p.mouse_mode);
        if self.mouse_mode != prev_mode {
            match self.mouse_mode {
                Mode::None => buf.extend_from_slice(match prev_mode {
                    Mode::None => b"",
                    Mode::Press => b"\x1b[?9l",
                    Mode::PressRelease => b"\x1b[?1000l",
                    Mode::ButtonMotion => b"\x1b[?1002l",
                    Mode::AnyMotion => b"\x1b[?1003l",
                }),
                Mode::Press => buf.extend_from_slice(b"\x1b[?9h"),
                Mode::PressRelease => buf.extend_from_slice(b"\x1b[?1000h"),
                Mode::ButtonMotion => buf.extend_from_slice(b"\x1b[?1002h"),
                Mode::AnyMotion => buf.extend_from_slice(b"\x1b[?1003h"),
            }
        }
        let prev_enc = prev.map_or(Enc::Default, |p| p.mouse_encoding);
        if self.mouse_encoding != prev_enc {
            match self.mouse_encoding {
                Enc::Default => buf.extend_from_slice(match prev_enc {
                    Enc::Default => b"",
                    Enc::Utf8 => b"\x1b[?1005l",
                    Enc::Sgr => b"\x1b[?1006l",
                }),
                Enc::Utf8 => buf.extend_from_slice(b"\x1b[?1005h"),
                Enc::Sgr => buf.extend_from_slice(b"\x1b[?1006h"),
            }
        }
        std::mem::take(buf)
    }
}
