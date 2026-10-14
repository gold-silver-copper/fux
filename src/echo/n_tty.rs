//! Linux's line discipline, `n_tty` (`drivers/tty/n_tty.c`): each function
//! here says which of its functions it follows.
use super::{Discipline, Lost, caret, continuation, is};
use fuxix::terminal::Modes;

/// Linux's `n_tty`, and the line it holds: what was typed since the last
/// line end, in line mode.
#[derive(Default, Debug)]
pub struct NTty {
    line: Vec<u8>,
}

/// How many bytes of echo the kernel may hold before it writes some of a
/// write's echo early (`ECHO_COMMIT_WATERMARK`).
const COMMIT_WATERMARK: usize = 256;

impl Discipline for NTty {
    fn input(&mut self, modes: &Modes, input: &[u8]) -> Result<Option<Vec<u8>>, Lost> {
        if modes.extproc || (modes.iuclc && modes.iexten) {
            return Err(Lost);
        }
        let mut write = Write {
            modes,
            out: Vec::new(),
            known: !(modes.opost && modes.olcuc),
            early: false,
            held: 0,
        };
        for &c in input {
            self.byte(&mut write, c)?;
        }
        Ok(write.known.then_some(write.out))
    }

    /// `n_tty_set_termios`: what was typed is committed, out of reach of
    /// an erase.
    fn mode_changed(&mut self) {
        self.line.clear();
    }

    fn program_wrote(&mut self) {}
}

impl NTty {
    /// Takes one typed byte (`n_tty_receive_buf_standard`).
    fn byte(&mut self, write: &mut Write, c: u8) -> Result<(), Lost> {
        let m = write.modes;
        let c = if m.istrip { c & 0x7f } else { c };
        if !special(m, c) {
            // `n_tty_receive_char`.
            if m.echo {
                write.typed(c);
            }
            self.queue(m, c);
            return Ok(());
        }
        // `n_tty_receive_char_special`.
        if m.ixon {
            if is(m.start, c) {
                write.flush_held();
                return Ok(());
            }
            if is(m.stop, c) {
                return Err(Lost);
            }
        }
        if m.isig && [m.intr, m.quit, m.susp].contains(&Some(c)) {
            self.signal(write, c);
            return Ok(());
        }
        let c = match c {
            b'\r' if m.igncr => return Ok(()),
            b'\r' if m.icrnl => b'\n',
            b'\n' if m.inlcr => b'\r',
            c => c,
        };
        if m.canonical && self.canonical(write, c)? {
            return Ok(());
        }
        if m.echo {
            if c == b'\n' {
                write.raw(b'\n');
            } else {
                write.typed(c);
            }
        }
        self.queue(m, c);
        Ok(())
    }

    /// Keeps `c` in the line, twice for a 0xff under `PARMRK`.
    fn queue(&mut self, m: &Modes, c: u8) {
        if m.canonical {
            self.line.push(c);
            if m.parmrk && c == 0xff {
                self.line.push(c);
            }
        }
    }

    /// An interrupt, quit or suspend character (`n_tty_receive_signal_char`
    /// and `isig`): unless `NOFLSH`, the line is thrown away, with the echo
    /// the kernel holds and what it wrote that the master has yet to take
    /// in (`pty_flush_buffer`). Echo this write wrote early may or may not
    /// be taken in by then.
    fn signal(&mut self, write: &mut Write, c: u8) {
        let m = write.modes;
        if !m.noflsh {
            if write.early || write.held >= COMMIT_WATERMARK {
                write.known = false;
            }
            write.out.clear();
            write.held = 0;
            self.line.clear();
        }
        if m.echo {
            write.char(c);
        } else {
            write.flush_held();
        }
    }

    /// The characters only line mode has (`n_tty_receive_char_canon`):
    /// whether `c` was one.
    fn canonical(&mut self, write: &mut Write, c: u8) -> Result<bool, Lost> {
        let m = write.modes;
        if is(m.erase, c) || is(m.kill, c) || (m.iexten && is(m.werase, c)) {
            self.erase(write, c)?;
        } else if m.iexten && is(m.lnext, c) {
            return Err(Lost);
        } else if m.iexten && m.echo && is(m.reprint, c) {
            write.char(c);
            write.raw(b'\n');
            for &c in &self.line {
                write.char(c);
            }
        } else if c == b'\n' {
            if m.echo || m.echonl {
                write.raw(b'\n');
            }
            self.line.clear();
        } else if is(m.eof, c) {
            self.line.clear();
        } else if is(m.eol, c) || (m.iexten && is(m.eol2, c)) {
            if m.echo {
                write.typed(c);
            }
            self.line.clear();
        } else {
            return Ok(false);
        }
        Ok(true)
    }

    /// An erase, word erase or kill (`eraser`).
    fn erase(&mut self, write: &mut Write, c: u8) -> Result<(), Lost> {
        let m = write.modes;
        if self.line.is_empty() {
            return Ok(());
        }
        let kind = if is(m.erase, c) {
            Erase::Char
        } else if is(m.werase, c) {
            Erase::Word
        } else if !m.echo {
            self.line.clear();
            return Ok(());
        } else if !(m.echok && m.echoke && m.echoe) {
            self.line.clear();
            write.char(c);
            if m.echok {
                write.raw(b'\n');
            }
            return Ok(());
        } else {
            Erase::Line
        };
        let mut seen_alnum = false;
        // A character's continuation bytes go with it; only continuation
        // bytes left are never erased.
        while let Some(at) = self
            .line
            .iter()
            .rposition(|&c| !(m.iutf8 && continuation(c)))
        {
            let Some(&first) = self.line.get(at) else {
                break;
            };
            if kind == Erase::Word {
                if alnum(first) {
                    seen_alnum = true;
                } else if seen_alnum {
                    break;
                }
            }
            self.line.truncate(at);
            if m.echo {
                if m.echoprt {
                    return Err(Lost);
                }
                if kind == Erase::Char && !m.echoe {
                    write.char(c);
                } else if first == b'\t' {
                    write.known = false;
                } else {
                    // A control character echoed as `^X` takes two columns,
                    // one echoed as itself none.
                    if cntrl(first) && m.echoctl {
                        write.rubout();
                    }
                    if !cntrl(first) || m.echoctl {
                        write.rubout();
                    }
                }
            }
            if kind == Erase::Char {
                break;
            }
        }
        Ok(())
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Erase {
    Char,
    Word,
    Line,
}

/// Whether `c` is one the kernel looks at before taking it as typed
/// (`n_tty_set_termios`'s `char_map`); never NUL, which a disabled
/// character is.
fn special(m: &Modes, c: u8) -> bool {
    let any = |chars: &[Option<u8>]| chars.contains(&Some(c));
    c != 0
        && ((c == b'\r' && (m.igncr || m.icrnl))
            || (c == b'\n' && m.inlcr)
            || (m.canonical
                && (c == b'\n'
                    || any(&[m.erase, m.kill, m.eof, m.eol])
                    || (m.iexten
                        && (any(&[m.werase, m.lnext, m.eol2]) || (m.echo && is(m.reprint, c))))))
            || (m.ixon && any(&[m.start, m.stop]))
            || (m.isig && any(&[m.intr, m.quit, m.susp])))
}

/// The kernel's `iscntrl`: ASCII's control characters only.
fn cntrl(c: u8) -> bool {
    c < 0x20 || c == 0x7f
}

/// The kernel's `isalnum`, or an underscore: ASCII's, and Latin-1's
/// letters, which every UTF-8 lead byte from 0xc0 reads as but two.
fn alnum(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_' || (c >= 0xc0 && c != 0xd7 && c != 0xf7)
}

/// One write being taken: its echo so far.
struct Write<'m> {
    modes: &'m Modes,
    out: Vec<u8>,
    /// False once some of the echo is outside the model.
    known: bool,
    /// Whether the kernel may have written some of `out` before the
    /// write's end.
    early: bool,
    /// At most how many bytes the kernel's echo buffer holds unwritten.
    held: usize,
}

impl Write<'_> {
    /// The kernel writes the echo it holds (`process_echoes`).
    fn flush_held(&mut self) {
        self.early |= !self.out.is_empty();
        self.held = 0;
    }

    /// Writes `c` through output processing (`do_output_char`).
    fn output(&mut self, c: u8) {
        let m = self.modes;
        if !m.opost {
            self.out.push(c);
            return;
        }
        match c {
            b'\n' if m.onlcr => self.out.extend_from_slice(b"\r\n"),
            // Both turn on the column, which the program's output moves
            // too.
            b'\r' if m.onocr => self.known = false,
            b'\t' if m.expand_tabs => self.known = false,
            b'\r' if m.ocrnl => self.out.push(b'\n'),
            c => self.out.push(c),
        }
    }

    /// Echoes `c` as it is (`echo_char_raw`).
    fn raw(&mut self, c: u8) {
        // 0xff is the echo buffer's escape, so it is held twice.
        self.held = self.held.saturating_add(if c == 0xff { 2 } else { 1 });
        self.output(c);
    }

    /// Echoes `c`, a control character as `^X` under `ECHOCTL`
    /// (`echo_char`); the pair skips output processing.
    fn char(&mut self, c: u8) {
        if self.modes.echoctl && cntrl(c) && c != b'\t' {
            self.held = self.held.saturating_add(2);
            self.out.extend_from_slice(&[b'^', caret(c)]);
        } else {
            self.raw(c);
        }
    }

    /// Echoes a character typed into the line, which may first record the
    /// line's column (`echo_set_canon_col`).
    fn typed(&mut self, c: u8) {
        self.held = self.held.saturating_add(2);
        self.char(c);
    }

    /// Erases a column.
    fn rubout(&mut self) {
        for c in *b"\x08 \x08" {
            self.raw(c);
        }
    }
}
