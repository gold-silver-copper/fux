//! Terminal output primitives for the viewer. Adapted from koh (MIT); see LICENSES/koh.txt.

use std::io::{self, Write};
use termina::{PlatformTerminal, Terminal as _};
use vt100::Color;

/// Every DEC private mode the viewer may have enabled on the user's terminal, reset together when
/// leaving the alternate screen so a shell never inherits mouse reporting or application keys.
pub const RESET_FORWARDED_MODES: &[u8] =
    b"\x1b[?9l\x1b[?2004l\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1005l\x1b[?1006l\x1b[?1l\x1b>";

#[derive(PartialEq, Eq, Clone, Copy)]
pub struct CellStyle {
    pub fg: Color,
    pub bg: Color,
    pub bold: bool,
    pub dim: bool,
    pub italic: bool,
    pub underline: bool,
    pub inverse: bool,
}

/// Platform primitives plus shared ANSI emission. Output is buffered until `flush`.
pub trait TerminalBackend {
    fn write_bytes(&mut self, bytes: &[u8]) -> io::Result<()>;
    fn flush(&mut self) -> io::Result<()>;
    fn enter_raw_mode(&mut self) -> io::Result<()>;
    fn leave_raw_mode(&mut self) -> io::Result<()>;
    fn size(&self) -> io::Result<(u16, u16)>;

    fn enter_alt_screen(&mut self) -> io::Result<()> {
        self.write_bytes(b"\x1b[?1049h\x1b[?25l")?;
        self.flush()
    }
    fn leave_alt_screen(&mut self) -> io::Result<()> {
        self.write_bytes(RESET_FORWARDED_MODES)?;
        self.write_bytes(b"\x1b[?25h\x1b[?1049l")?;
        self.flush()
    }
    /// DEC synchronized output (mode 2026) so a whole frame appears at once.
    fn begin_frame(&mut self) -> io::Result<()> {
        self.write_bytes(b"\x1b[?2026h\x1b[?25l")
    }
    fn end_frame(&mut self) -> io::Result<()> {
        self.write_bytes(b"\x1b[?2026l")
    }
    fn move_to(&mut self, row: u16, col: u16) -> io::Result<()> {
        self.write_bytes(format!("\x1b[{};{}H", u32::from(row) + 1, u32::from(col) + 1).as_bytes())
    }
    fn set_style(&mut self, style: CellStyle) -> io::Result<()> {
        self.write_bytes(b"\x1b[m")?;
        if style.bold {
            self.write_bytes(b"\x1b[1m")?;
        }
        if style.dim {
            self.write_bytes(b"\x1b[2m")?;
        }
        if style.italic {
            self.write_bytes(b"\x1b[3m")?;
        }
        if style.underline {
            self.write_bytes(b"\x1b[4m")?;
        }
        if style.inverse {
            self.write_bytes(b"\x1b[7m")?;
        }
        write_sgr_color(self, style.fg, true)?;
        write_sgr_color(self, style.bg, false)
    }
    fn print(&mut self, glyph: &str) -> io::Result<()> {
        self.write_bytes(glyph.as_bytes())
    }
    fn show_cursor(&mut self) -> io::Result<()> {
        self.write_bytes(b"\x1b[?25h")
    }
    fn set_window_title(&mut self, title: &str) -> io::Result<()> {
        self.write_bytes(format!("\x1b]0;{title}\x07").as_bytes())
    }
    /// The caller has already applied the clipboard policy and validated `base64`.
    fn set_clipboard(&mut self, base64: &str) -> io::Result<()> {
        self.write_bytes(format!("\x1b]52;c;{base64}\x07").as_bytes())
    }
    fn write_input_modes(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.write_bytes(bytes)
    }
}

fn write_sgr_color(
    out: &mut (impl TerminalBackend + ?Sized),
    color: Color,
    fg: bool,
) -> io::Result<()> {
    match color {
        Color::Default => out.write_bytes(if fg { b"\x1b[39m" } else { b"\x1b[49m" }),
        Color::Idx(index) if index < 8 => {
            let base: u16 = if fg { 30 } else { 40 };
            out.write_bytes(format!("\x1b[{}m", base + u16::from(index)).as_bytes())
        }
        Color::Idx(index) if index < 16 => {
            let base: u16 = if fg { 82 } else { 92 };
            out.write_bytes(format!("\x1b[{}m", base + u16::from(index)).as_bytes())
        }
        Color::Idx(index) => {
            let lead = if fg { 38 } else { 48 };
            out.write_bytes(format!("\x1b[{lead};5;{index}m").as_bytes())
        }
        Color::Rgb(r, g, b) => {
            let lead = if fg { 38 } else { 48 };
            out.write_bytes(format!("\x1b[{lead};2;{r};{g};{b}m").as_bytes())
        }
    }
}

/// The real terminal through `termina`.
pub struct TerminaBackend {
    term: PlatformTerminal,
}

impl TerminaBackend {
    pub fn new() -> io::Result<Self> {
        Ok(Self {
            term: PlatformTerminal::new()?,
        })
    }
}

impl TerminalBackend for TerminaBackend {
    fn write_bytes(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.term.write_all(bytes)
    }
    fn flush(&mut self) -> io::Result<()> {
        Write::flush(&mut self.term)
    }
    fn enter_raw_mode(&mut self) -> io::Result<()> {
        self.term.enter_raw_mode()
    }
    fn leave_raw_mode(&mut self) -> io::Result<()> {
        self.term.enter_cooked_mode()
    }
    fn size(&self) -> io::Result<(u16, u16)> {
        let dimensions = self.term.get_dimensions()?;
        Ok((dimensions.rows, dimensions.cols))
    }
}

/// An in-memory backend that captures every emitted byte for tests.
#[derive(Debug)]
pub struct CaptureBackend {
    pub bytes: Vec<u8>,
    pub rows: u16,
    pub cols: u16,
    pub raw: bool,
    pub alternate: bool,
    pub flushes: usize,
}

impl CaptureBackend {
    #[must_use]
    pub fn new(rows: u16, cols: u16) -> Self {
        Self {
            bytes: Vec::new(),
            rows,
            cols,
            raw: false,
            alternate: false,
            flushes: 0,
        }
    }
}

impl TerminalBackend for CaptureBackend {
    fn write_bytes(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.bytes.extend_from_slice(bytes);
        Ok(())
    }
    fn flush(&mut self) -> io::Result<()> {
        self.flushes = self.flushes.saturating_add(1);
        Ok(())
    }
    fn enter_raw_mode(&mut self) -> io::Result<()> {
        self.raw = true;
        Ok(())
    }
    fn leave_raw_mode(&mut self) -> io::Result<()> {
        self.raw = false;
        Ok(())
    }
    fn size(&self) -> io::Result<(u16, u16)> {
        Ok((self.rows, self.cols))
    }
    fn enter_alt_screen(&mut self) -> io::Result<()> {
        self.alternate = true;
        self.write_bytes(b"\x1b[?1049h\x1b[?25l")?;
        self.flush()
    }
    fn leave_alt_screen(&mut self) -> io::Result<()> {
        self.alternate = false;
        self.write_bytes(RESET_FORWARDED_MODES)?;
        self.write_bytes(b"\x1b[?25h\x1b[?1049l")?;
        self.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn emit(f: impl FnOnce(&mut CaptureBackend) -> io::Result<()>) -> Vec<u8> {
        let mut backend = CaptureBackend::new(24, 80);
        let _ = f(&mut backend);
        backend.bytes
    }

    #[test]
    fn sgr_colors_use_theme_aware_codes() {
        assert_eq!(
            emit(|b| write_sgr_color(b, Color::Idx(1), true)),
            b"\x1b[31m"
        );
        assert_eq!(
            emit(|b| write_sgr_color(b, Color::Idx(8), true)),
            b"\x1b[90m"
        );
        assert_eq!(
            emit(|b| write_sgr_color(b, Color::Idx(196), false)),
            b"\x1b[48;5;196m"
        );
        assert_eq!(
            emit(|b| write_sgr_color(b, Color::Rgb(0, 0, 255), true)),
            b"\x1b[38;2;0;0;255m"
        );
        assert_eq!(
            emit(|b| write_sgr_color(b, Color::Default, false)),
            b"\x1b[49m"
        );
    }

    #[test]
    fn move_to_is_one_based_and_overflow_safe() {
        assert_eq!(emit(|b| b.move_to(0, 0)), b"\x1b[1;1H");
        assert_eq!(emit(|b| b.move_to(u16::MAX, 0)), b"\x1b[65536;1H");
    }

    #[test]
    fn leaving_the_alternate_screen_resets_forwarded_modes() {
        let bytes = emit(TerminalBackend::leave_alt_screen);
        assert!(bytes.starts_with(RESET_FORWARDED_MODES));
        assert!(bytes.ends_with(b"\x1b[?25h\x1b[?1049l"));
    }
}
