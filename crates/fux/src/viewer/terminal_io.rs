//! The outer terminal: raw mode, alternate screen, mouse reporting, bracketed paste and focus
//! tracking through termina, restored on drop and from the panic hook (chained before the default
//! hook, prompt section 2). Input events, including `SIGWINCH` (termina's own signal pipe), are
//! read on a thread and pushed to the runner's wake channel.

use std::io::{self, Write};
use std::sync::mpsc::Sender;
use std::thread::JoinHandle;

use termina::escape::csi::{
    Csi, DecPrivateMode, DecPrivateModeCode, Edit, EraseInDisplay, Mode, Sgr,
};
use termina::{PlatformTerminal, Terminal};

use super::Wake;

fn dec(code: DecPrivateModeCode, set: bool) -> Csi {
    let mode = DecPrivateMode::Code(code);
    Csi::Mode(if set {
        Mode::SetDecPrivateMode(mode)
    } else {
        Mode::ResetDecPrivateMode(mode)
    })
}

fn write_enter(out: &mut impl Write) -> io::Result<()> {
    write!(
        out,
        "{}{}{}{}{}{}{}",
        dec(DecPrivateModeCode::ClearAndEnableAlternateScreen, true),
        dec(DecPrivateModeCode::BracketedPaste, true),
        dec(DecPrivateModeCode::ButtonEventMouse, true),
        dec(DecPrivateModeCode::SGRMouse, true),
        dec(DecPrivateModeCode::FocusTracking, true),
        dec(DecPrivateModeCode::ShowCursor, false),
        Csi::Edit(Edit::EraseInDisplay(EraseInDisplay::EraseDisplay)),
    )
}

fn write_leave(out: &mut impl Write) -> io::Result<()> {
    write!(
        out,
        "{}{}{}{}{}{}{}",
        Csi::Sgr(Sgr::Reset),
        dec(DecPrivateModeCode::ShowCursor, true),
        dec(DecPrivateModeCode::FocusTracking, false),
        dec(DecPrivateModeCode::SGRMouse, false),
        dec(DecPrivateModeCode::ButtonEventMouse, false),
        dec(DecPrivateModeCode::BracketedPaste, false),
        dec(DecPrivateModeCode::ClearAndEnableAlternateScreen, false),
    )
}

/// The process terminal in application mode.
pub struct TerminalIo {
    term: PlatformTerminal,
    restored: bool,
}

impl TerminalIo {
    /// Opens the terminal, installs the restoring panic hook and enters application mode.
    pub fn open() -> io::Result<Self> {
        let mut term = PlatformTerminal::new()?;
        term.set_panic_hook(|handle| {
            let _ = write_leave(handle);
            let _ = handle.flush();
        });
        term.enter_raw_mode()?;
        write_enter(&mut term)?;
        term.flush()?;
        Ok(Self {
            term,
            restored: false,
        })
    }

    /// `(cols, rows)`.
    pub fn size(&self) -> io::Result<(u16, u16)> {
        let size = self.term.get_dimensions()?;
        Ok((size.cols, size.rows))
    }

    /// Reads events until the terminal closes, pushing each as `Wake::Terminal`.
    pub fn spawn_reader(&self, wake: Sender<Wake>) -> io::Result<JoinHandle<()>> {
        let reader = self.term.event_reader();
        std::thread::Builder::new()
            .name("fux-terminal-reader".into())
            .spawn(move || {
                loop {
                    match reader.read(|_| true) {
                        Ok(event) => {
                            if wake.send(Wake::Terminal(event)).is_err() {
                                break;
                            }
                        }
                        Err(_) => {
                            let _ = wake.send(Wake::TerminalClosed);
                            break;
                        }
                    }
                }
            })
    }

    pub fn write_all(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.term.write_all(bytes)?;
        self.term.flush()
    }

    /// Leaves application mode and returns the terminal to cooked mode.
    pub fn restore(&mut self) -> io::Result<()> {
        if self.restored {
            return Ok(());
        }
        self.restored = true;
        write_leave(&mut self.term)?;
        self.term.flush()?;
        self.term.enter_cooked_mode()
    }
}

impl Drop for TerminalIo {
    fn drop(&mut self) {
        if !std::thread::panicking() {
            let _ = self.restore();
        }
    }
}
