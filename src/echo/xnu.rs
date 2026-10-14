//! macOS's line discipline, the BSD one in xnu's `bsd/kern/tty.c`: each
//! function here says which of its functions it follows.
use super::{Discipline, Lost, caret, continuation, is};
use fuxix::terminal::Modes;

/// macOS's line discipline, and the line it holds: what was typed since
/// the last line end, in line mode (the raw queue).
#[derive(Default, Debug)]
pub struct Xnu {
    line: Vec<u8>,
    /// How many of the line's characters were echoed since the program
    /// last wrote (`t_rocount`): an erase with fewer to go retypes the
    /// line, and a kill erases it only when all were.
    rocount: isize,
}

/// How many bytes the terminal's output queue holds.
const OUTPUT_QUEUE: usize = 1024;

impl Discipline for Xnu {
    fn input(&mut self, modes: &Modes, input: &[u8]) -> Result<Option<Vec<u8>>, Lost> {
        if modes.extproc {
            return Err(Lost);
        }
        let mut write = Write {
            modes,
            out: Vec::new(),
            known: true,
        };
        for &c in input {
            self.byte(&mut write, c)?;
        }
        // The output queue holds 1024 bytes (`TTYCLSIZE`), and echo past
        // that is dropped.
        Ok((write.known && write.out.len() <= OUTPUT_QUEUE).then_some(write.out))
    }

    /// The `ICANON` change in `ttioctl_locked`: what was typed becomes
    /// input to read; the program is taken to have read it before it
    /// turns line mode back on, so the pending input (`PENDIN`) the kernel
    /// would take again is none.
    fn mode_changed(&mut self) {
        self.line.clear();
    }

    /// `ttwrite`.
    fn program_wrote(&mut self) {
        self.rocount = 0;
    }
}

impl Xnu {
    /// Takes one typed byte (`ttyinput`).
    fn byte(&mut self, write: &mut Write, c: u8) -> Result<(), Lost> {
        let m = write.modes;
        let c = if m.istrip { c & 0x7f } else { c };
        if m.iexten && (is(m.lnext, c) || is(m.discard, c)) {
            return Err(Lost);
        }
        if m.isig {
            if is(m.intr, c) || is(m.quit, c) {
                // `ttyflush(FREAD | FWRITE)`: the line, and the echo the
                // master has not read.
                if !m.noflsh {
                    write.out.clear();
                    self.flush();
                }
                write.echo(c);
                return Ok(());
            }
            if is(m.susp, c) {
                if !m.noflsh {
                    self.flush();
                }
                write.echo(c);
                return Ok(());
            }
        }
        if m.ixon {
            if is(m.stop, c) {
                return Err(Lost);
            }
            if is(m.start, c) {
                return Ok(());
            }
        }
        let c = match c {
            b'\r' if m.igncr => return Ok(()),
            b'\r' if m.icrnl => b'\n',
            b'\n' if m.inlcr => b'\r',
            c => c,
        };
        if !m.canonical {
            write.echo(c);
            return Ok(());
        }
        if self.edit(write, c)? {
            return Ok(());
        }
        if m.parmrk && c == 0xff {
            return Err(Lost);
        }
        if c == b'\n' || is(m.eof, c) || is(m.eol, c) || (m.iexten && is(m.eol2, c)) {
            self.line.clear();
            self.rocount = 0;
        } else {
            self.line.push(c);
            self.rocount = self.rocount.saturating_add(1);
        }
        write.echo(c);
        if is(m.eof, c) && m.echo {
            // The cursor goes back over the `^` of `^D`: as many columns
            // as the echo moved, up to two.
            let back = if !m.opost {
                0
            } else if m.echoctl && shown_as_caret(c) {
                2
            } else {
                match class(c) {
                    Class::Ordinary => 1,
                    Class::Tab => {
                        write.known = false;
                        0
                    }
                    Class::Control => 0,
                }
            };
            for _ in 0..back {
                write.output(b'\x08');
            }
        }
        Ok(())
    }

    /// `ttyflush(FREAD)`'s part in the line.
    fn flush(&mut self) {
        self.line.clear();
        self.rocount = 0;
    }

    /// The line's length, as `rocount` counts.
    fn count(&self) -> isize {
        isize::try_from(self.line.len()).unwrap_or(isize::MAX)
    }

    /// The editing characters of line mode: whether `c` was one.
    fn edit(&mut self, write: &mut Write, c: u8) -> Result<bool, Lost> {
        let m = write.modes;
        if is(m.erase, c) {
            // A character's continuation bytes go with it.
            while let Some(c) = self.line.pop() {
                self.rub(write, c)?;
                if !(m.iutf8 && continuation(c)) {
                    break;
                }
            }
        } else if is(m.kill, c) {
            if m.echoke && self.count() == self.rocount && !m.echoprt {
                while let Some(c) = self.line.pop() {
                    self.rub(write, c)?;
                }
            } else {
                write.echo(c);
                if m.echok || m.echoke {
                    write.echo(b'\n');
                }
                self.flush();
            }
        } else if m.iexten && is(m.werase, c) {
            self.erase_word(write)?;
        } else if m.iexten && is(m.reprint, c) {
            self.retype(write);
        } else if m.iexten && is(m.status, c) {
            // A SIGINFO, and the kernel's line about the foreground
            // process, which usually, not always, resets `rocount`.
            return Err(Lost);
        } else {
            return Ok(false);
        }
        Ok(true)
    }

    /// A word erase: the blanks before the cursor, then the word.
    fn erase_word(&mut self, write: &mut Write) -> Result<(), Lost> {
        let blank = |c| c == b' ' || c == b'\t';
        let mut c = loop {
            match self.line.pop() {
                None => return Ok(()),
                Some(c) if blank(c) => self.rub(write, c)?,
                Some(c) => break c,
            }
        };
        self.rub(write, c)?;
        // With `ALTWERASE`, a word ends where letters and digits meet
        // other characters too, as the one before the last says.
        let Some(next) = self.line.pop() else {
            return Ok(());
        };
        if blank(next) {
            self.line.push(next);
            return Ok(());
        }
        c = next;
        let kind = alpha(c);
        loop {
            self.rub(write, c)?;
            let Some(next) = self.line.pop() else {
                return Ok(());
            };
            if blank(next) || (write.modes.altwerase && alpha(next) != kind) {
                self.line.push(next);
                return Ok(());
            }
            c = next;
        }
    }

    /// Erases `c`, just taken off the line, from the screen (`ttyrub`).
    fn rub(&mut self, write: &mut Write, c: u8) -> Result<(), Lost> {
        let m = write.modes;
        if !m.echo {
            return Ok(());
        }
        if m.echoe {
            if self.rocount == 0 {
                self.retype(write);
                return Ok(());
            }
            match class(c) {
                Class::Ordinary => {
                    if !(m.iutf8 && continuation(c)) {
                        write.rubout(1);
                    }
                }
                Class::Tab => {
                    if self.rocount < self.count() {
                        self.retype(write);
                        return Ok(());
                    }
                    write.known = false;
                }
                Class::Control => {
                    if m.echoctl {
                        write.rubout(2);
                    }
                }
            }
        } else if m.echoprt {
            return Err(Lost);
        } else {
            // A disabled erase character is `_POSIX_VDISABLE`, echoed too.
            write.echo(m.erase.unwrap_or(0xff));
        }
        self.rocount = self.rocount.saturating_sub(1);
        Ok(())
    }

    /// Reprints the line (`ttyretype`), with the lines typed before it
    /// that the program has not read, which are out of the model's sight.
    fn retype(&mut self, write: &mut Write) {
        write.known = false;
        self.rocount = self.count();
    }
}

/// How a character moves the column, as `char_type` classes it: backspace,
/// newline, return and vertical tab are taken with the control characters,
/// as erasing them is.
enum Class {
    Ordinary,
    Tab,
    Control,
}

fn class(c: u8) -> Class {
    match c {
        b'\t' => Class::Tab,
        0..0x20 | 0x7f => Class::Control,
        _ => Class::Ordinary,
    }
}

/// Whether `ECHOCTL` echoes `c` as `^X` (`ttyecho`).
fn shown_as_caret(c: u8) -> bool {
    (c < 0x20 && c != b'\t' && c != b'\n') || c == 0x7f
}

/// Whether `char_type` has `c` as alphanumeric (`ISALPHA`): ASCII's
/// letters, digits and underscore, and every byte above ASCII.
fn alpha(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_' || c >= 0x80
}

/// One write being taken: its echo so far.
struct Write<'m> {
    modes: &'m Modes,
    out: Vec<u8>,
    /// False once some of the echo is outside the model.
    known: bool,
}

impl Write<'_> {
    /// Writes `c` through output processing (`ttyoutput`).
    fn output(&mut self, c: u8) {
        let m = self.modes;
        if !m.opost {
            self.out.push(c);
            return;
        }
        match c {
            // Both turn on the column, which the program's output moves
            // too.
            b'\t' if m.expand_tabs => self.known = false,
            0x04 if m.onoeot => {}
            b'\n' if m.onlcr => self.out.extend_from_slice(b"\r\n"),
            b'\r' if m.ocrnl => self.out.push(b'\n'),
            b'\r' if m.onocr => self.known = false,
            c => self.out.push(c),
        }
    }

    /// Echoes `c`, a control character as `^X` under `ECHOCTL`
    /// (`ttyecho`); without `ECHO`, only a newline under `ECHONL`.
    fn echo(&mut self, c: u8) {
        let m = self.modes;
        if !m.echo && !(c == b'\n' && m.echonl) {
            return;
        }
        if m.echoctl && shown_as_caret(c) {
            self.output(b'^');
            self.output(caret(c));
        } else {
            self.output(c);
        }
    }

    /// Erases `columns` columns (`ttyrubo`).
    fn rubout(&mut self, columns: usize) {
        for _ in 0..columns {
            for c in *b"\x08 \x08" {
                self.output(c);
            }
        }
    }
}
