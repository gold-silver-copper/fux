//! The viewer's terminal: raw mode and alternate screen lifecycle, frame painting, window title,
//! clipboard writes under policy, and forwarded input modes. Everything is restored on drop.

use super::backend::{CaptureBackend, TerminaBackend, TerminalBackend};
use super::hints::HintPanel;
use super::render::{LocalView, Notice, Palette, compose, paint};
use crate::view::{Frame, MouseEncoding, MouseMode, PaneModes};
use base64::Engine as _;
use ratatui_core::buffer::Buffer;
use std::io;

pub const MAX_CLIPBOARD_BYTES: usize = 1 << 20;
const MAX_TITLE_BYTES: usize = 4096;

pub struct Screen<B: TerminalBackend> {
    backend: B,
    previous: Option<Buffer>,
    clipboard_enabled: bool,
    last_title: Option<String>,
    previous_modes: Option<PaneModes>,
    palette: Palette,
}

impl Screen<TerminaBackend> {
    pub fn enter_default(clipboard_enabled: bool, palette: Palette) -> io::Result<Self> {
        Self::enter(TerminaBackend::new()?, clipboard_enabled, palette)
    }
}

impl Screen<CaptureBackend> {
    pub fn capture(rows: u16, cols: u16, clipboard_enabled: bool) -> io::Result<Self> {
        Self::enter(
            CaptureBackend::new(rows, cols),
            clipboard_enabled,
            Palette::default(),
        )
    }
    pub fn bytes(&self) -> &[u8] {
        &self.backend.bytes
    }
}

impl<B: TerminalBackend> Screen<B> {
    pub fn enter(mut backend: B, clipboard_enabled: bool, palette: Palette) -> io::Result<Self> {
        backend.enter_raw_mode()?;
        if let Err(error) = backend.enter_alt_screen() {
            let _ = backend.end_frame();
            let _ = backend.leave_alt_screen();
            let _ = backend.leave_raw_mode();
            return Err(error);
        }
        // fux always sees SGR any-motion mouse reports so it can route them per pane.
        backend.write_input_modes(b"\x1b[?1003h\x1b[?1006h")?;
        backend.flush()?;
        Ok(Self {
            backend,
            previous: None,
            clipboard_enabled,
            last_title: None,
            previous_modes: None,
            palette,
        })
    }

    pub fn size(&self) -> io::Result<(u16, u16)> {
        self.backend.size()
    }

    /// Forget the previous buffer so the next paint clears and redraws everything.
    pub fn invalidate(&mut self) {
        self.previous = None;
        self.last_title = None;
        self.previous_modes = None;
    }

    pub fn render(
        &mut self,
        frame: &Frame,
        local: Option<&LocalView<'_>>,
        panel: Option<&HintPanel>,
        notice: Option<&Notice>,
    ) -> io::Result<()> {
        self.emit_out_of_band(frame)?;
        let (rows, cols) = self.backend.size()?;
        let composed = compose(frame, local, panel, notice, &self.palette, rows, cols);
        paint(
            &mut self.backend,
            self.previous.as_ref(),
            &composed.buffer,
            composed.cursor,
        )?;
        self.previous = Some(composed.buffer);
        Ok(())
    }

    fn emit_out_of_band(&mut self, frame: &Frame) -> io::Result<()> {
        let title = frame
            .focused_pane()
            .map(|pane| sanitize_title(&pane.title))
            .unwrap_or_default();
        if self.last_title.as_deref() != Some(title.as_str()) {
            self.backend.set_window_title(&title)?;
            self.last_title = Some(title);
        }
        let modes = frame
            .focused_pane()
            .map(|pane| pane.modes)
            .unwrap_or_default();
        let bytes = input_mode_bytes(self.previous_modes, modes);
        if !bytes.is_empty() {
            self.backend.write_input_modes(&bytes)?;
        }
        self.previous_modes = Some(modes);
        Ok(())
    }

    /// Writes a viewer copy to the terminal clipboard when policy allows. Returns false when the
    /// policy forbids it or the payload is invalid.
    pub fn copy_to_clipboard(&mut self, text: &str) -> io::Result<bool> {
        if !self.clipboard_enabled {
            return Ok(false);
        }
        let encoded = base64::engine::general_purpose::STANDARD.encode(text);
        if encoded.is_empty() || encoded.len() > MAX_CLIPBOARD_BYTES {
            return Ok(false);
        }
        self.backend.set_clipboard(&encoded)?;
        self.backend.flush()?;
        Ok(true)
    }

    pub fn clipboard_enabled(&self) -> bool {
        self.clipboard_enabled
    }

    pub fn leave_for_suspend(&mut self) -> io::Result<()> {
        self.backend.end_frame()?;
        self.backend.leave_alt_screen()?;
        self.backend.leave_raw_mode()
    }

    pub fn reenter_after_resume(&mut self) -> io::Result<()> {
        self.backend.enter_raw_mode()?;
        self.backend.enter_alt_screen()?;
        self.backend.write_input_modes(b"\x1b[?1003h\x1b[?1006h")?;
        self.invalidate();
        Ok(())
    }
}

impl<B: TerminalBackend> Drop for Screen<B> {
    fn drop(&mut self) {
        let _ = self.backend.end_frame();
        let _ = self.backend.leave_alt_screen();
        let _ = self.backend.leave_raw_mode();
    }
}

fn sanitize_title(title: &str) -> String {
    let mut output = String::new();
    for character in title.chars().filter(|value| !value.is_control()) {
        if output.len().saturating_add(character.len_utf8()) > MAX_TITLE_BYTES {
            break;
        }
        output.push(character);
    }
    output
}

/// Escape sequences moving the terminal from `previous` to `next` application modes. Mouse
/// reporting is owned by fux and never changed here.
pub fn input_mode_bytes(previous: Option<PaneModes>, next: PaneModes) -> Vec<u8> {
    let mut bytes = Vec::new();
    let changed =
        |get: fn(&PaneModes) -> bool| previous.is_none_or(|previous| get(&previous) != get(&next));
    if changed(|modes| modes.application_keypad) {
        bytes.extend_from_slice(if next.application_keypad {
            b"\x1b="
        } else {
            b"\x1b>"
        });
    }
    if changed(|modes| modes.application_cursor) {
        bytes.extend_from_slice(if next.application_cursor {
            b"\x1b[?1h"
        } else {
            b"\x1b[?1l"
        });
    }
    if changed(|modes| modes.bracketed_paste) {
        bytes.extend_from_slice(if next.bracketed_paste {
            b"\x1b[?2004h"
        } else {
            b"\x1b[?2004l"
        });
    }
    let _ = (MouseMode::None, MouseEncoding::Default);
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn input_modes_emit_only_changes() {
        let mut modes = PaneModes::default();
        let first = input_mode_bytes(None, modes);
        assert!(first.starts_with(b"\x1b>"));
        assert!(first.ends_with(b"\x1b[?2004l"));
        modes.application_cursor = true;
        assert_eq!(
            input_mode_bytes(Some(PaneModes::default()), modes),
            b"\x1b[?1h"
        );
        assert!(input_mode_bytes(Some(modes), modes).is_empty());
    }

    #[test]
    fn screen_restores_terminal_on_drop_and_gates_clipboard() {
        let mut screen = Screen::capture(4, 10, false).unwrap_or_else(|_| unreachable_screen());
        assert!(screen.render(&Frame::default(), None, None, None).is_ok());
        assert!(!screen.copy_to_clipboard("hi").unwrap_or(true));
        let mut allowed = Screen::capture(4, 10, true).unwrap_or_else(|_| unreachable_screen());
        assert!(allowed.copy_to_clipboard("hi").unwrap_or(false));
        assert!(String::from_utf8_lossy(allowed.bytes()).contains("\x1b]52;c;aGk=\x07"));
        let huge = "x".repeat(MAX_CLIPBOARD_BYTES);
        assert!(!allowed.copy_to_clipboard(&huge).unwrap_or(true));
        let bytes_before = screen.bytes().len();
        drop(screen);
        let _ = bytes_before;
    }

    fn unreachable_screen() -> Screen<CaptureBackend> {
        Screen {
            backend: CaptureBackend::new(1, 1),
            previous: None,
            clipboard_enabled: false,
            last_title: None,
            previous_modes: None,
            palette: Palette::default(),
        }
    }
}
