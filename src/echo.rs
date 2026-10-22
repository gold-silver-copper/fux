//! What the kernel echoes as fux types into a pane. In line mode the
//! kernel, not the program, echoes the keys as they come; [`Echo`] works
//! out those bytes, as the kernel writes them back to the pty's master, so
//! that the echo of typed keys can be told apart from the program's own
//! output.
//!
//! Linux's `n_tty` (`drivers/tty/n_tty.c`) and the BSD rules macOS keeps
//! (xnu's `bsd/kern/tty.c`) agree on what is modelled here, but for the few
//! rules that follow from [`MACOS`].
//!
//! The model covers text, carriage returns and newlines (`ICRNL`, `INLCR`,
//! `IGNCR`, `ONLCR`, `OCRNL`), `ECHO`, `ECHONL`, `ECHOCTL`, end of line and
//! file, and the signal characters. For the rest it answers `None`, rather
//! than guess: the caller then tells echo by order. That is line editing
//! (erase, word erase, kill), literal next, reprint, discard, status, flow
//! control, tabs written as spaces, `ONOCR`, and modes that rewrite input or
//! output (`EXTPROC`, `IUCLC`, `OLCUC`, `ALTWERASE`) and `PARMRK`'s 0xff. fux reads
//! the master's echo as it comes, so an interrupt's flush finds none of it
//! left over from earlier writes.
use fuxix::terminal::Modes;

/// Whether this build runs on macOS's BSD line discipline rather than
/// Linux's `n_tty`.
const MACOS: bool = cfg!(target_os = "macos");

/// The echo of what is typed into one terminal.
#[derive(Default, Debug)]
pub struct Echo {
    /// How many bytes were typed since the last line end, in line mode.
    line: usize,
    /// Whether line editing was on at the last write.
    canonical: Option<bool>,
}

/// Echo past this many bytes of one write: macOS's output queue holds 1024
/// (`TTYCLSIZE`), and the rest is dropped.
const OUTPUT_QUEUE: usize = 1024;
/// A line longer than this may overflow the kernel's input queue.
const LONGEST_LINE: usize = 1000;
/// An interrupt flushes the echo of the write it is in, when the kernel
/// holds all of it then: Linux writes its echo early from 256 bytes of its
/// buffer (`ECHO_COMMIT_WATERMARK`), which holds at most 3 per byte echoed.
const FLUSHED_WHOLE: usize = 80;

impl Echo {
    /// What the kernel echoes as `input` is written under `modes`, after
    /// output processing (`OPOST`, `ONLCR`...). `None` when some byte or
    /// mode is outside the model: the caller then falls back.
    pub fn echo(&mut self, modes: &Modes, input: &[u8]) -> Option<Vec<u8>> {
        // Turning line mode on or off commits what was typed.
        if self.canonical.replace(modes.canonical) != Some(modes.canonical) {
            self.line = 0;
        }
        let m = modes;
        if m.extproc || m.iuclc || m.altwerase {
            return None;
        }
        let mut write = Write {
            modes,
            out: Vec::new(),
            known: !(m.opost && m.olcuc),
        };
        for &c in input {
            self.byte(&mut write, c)?;
        }
        (write.known && write.out.len() <= OUTPUT_QUEUE).then_some(write.out)
    }

    /// Takes one typed byte (`n_tty_receive_buf_standard`, `ttyinput`):
    /// `None` when it is outside the model.
    fn byte(&mut self, w: &mut Write, typed: u8) -> Option<()> {
        let m = w.modes;
        let typed = if m.istrip { typed & 0x7f } else { typed };
        // Linux edits the line and takes literal next and reprint only in
        // line mode; macOS takes literal next, discard and status in any.
        let left_out = |c| {
            let is = |chars: &[Option<u8>]| chars.contains(&Some(c));
            (m.parmrk && c == 0xff)
                || (m.ixon && is(&[m.start, m.stop]))
                || (m.canonical && is(&[m.erase, m.kill]))
                || (m.iexten && m.canonical && is(&[m.werase, m.reprint]))
                || (m.iexten && (m.canonical || MACOS) && is(&[m.lnext, m.discard, m.status]))
        };
        if left_out(typed) {
            return None;
        }
        if m.isig && [m.intr, m.quit, m.susp].contains(&Some(typed)) {
            // Unless `NOFLSH`, the line is thrown away, and with it the
            // echo not yet read.
            if !m.noflsh {
                self.line = 0;
                // On macOS a suspend keeps the echo not yet read.
                if !MACOS || m.intr == Some(typed) || m.quit == Some(typed) {
                    w.known &= w.out.len() < FLUSHED_WHOLE;
                    w.out.clear();
                }
            }
            w.echo(typed);
            return Some(());
        }
        let c = match typed {
            b'\r' if m.igncr => return Some(()),
            b'\r' if m.icrnl => b'\n',
            b'\n' if m.inlcr => b'\r',
            c => c,
        };
        if left_out(c) {
            return None;
        }
        if !m.canonical {
            match c == b'\n' && typed != c {
                // Linux echoes `ICRNL`'s newline as it is, but only under
                // `ECHO`; macOS under `ECHONL` too.
                true if m.echo => w.output(b'\n'),
                true => w.known &= !m.echonl,
                false => w.echo(c),
            }
            return Some(());
        }
        if c == b'\n' || m.eof == Some(c) || m.eol == Some(c) || (m.iexten && m.eol2 == Some(c)) {
            self.line = 0;
        } else if self.line < LONGEST_LINE {
            self.line = self.line.saturating_add(1);
        } else {
            return None;
        }
        if c == b'\n' {
            if m.echo || m.echonl {
                w.output(b'\n');
            }
        } else if m.eof != Some(c) {
            w.echo(c);
        } else if MACOS {
            // Only macOS echoes the end-of-file character.
            w.eof(c);
        }
        Some(())
    }
}

/// One write being taken: its echo so far.
struct Write<'m> {
    modes: &'m Modes,
    out: Vec<u8>,
    /// False once some of the echo is outside the model.
    known: bool,
}

impl Write<'_> {
    /// Writes `c` through output processing (`do_output_char`,
    /// `ttyoutput`).
    fn output(&mut self, c: u8) {
        let m = self.modes;
        match c {
            _ if !m.opost => self.out.push(c),
            b'\n' if m.onlcr => self.out.extend_from_slice(b"\r\n"),
            // Both turn on the column, which the program's output moves
            // too.
            b'\r' if m.onocr => self.known = false,
            b'\t' if m.expand_tabs => self.known = false,
            b'\r' if m.ocrnl => self.out.push(b'\n'),
            0x04 if m.onoeot => {}
            c => self.out.push(c),
        }
    }

    /// Echoes `c`, a control character as `^X` under `ECHOCTL`
    /// (`echo_char`, `ttyecho`); the pair skips output processing.
    fn echo(&mut self, c: u8) {
        let m = self.modes;
        if c == b'\n' && !m.echo {
            // macOS echoes it under `ECHONL` alone.
            self.known &= !m.echonl;
        // Only Linux echoes a newline that ends no line as `^J`.
        } else if m.echo && m.echoctl && caret(c) && (c != b'\n' || !MACOS) {
            self.out.extend_from_slice(&[b'^', c ^ 0x40]);
        } else if m.echo {
            self.output(c);
        }
    }

    /// macOS's echo of the end-of-file character: the cursor goes back
    /// over it as many columns as it moved, up to two.
    fn eof(&mut self, c: u8) {
        self.echo(c);
        let m = self.modes;
        let back = match c {
            _ if !(m.echo && m.opost) => 0,
            _ if m.echoctl && caret(c) => 2,
            b' '..=b'~' => 1,
            b'\t' | 0x80.. => {
                self.known = false;
                0
            }
            _ => 0,
        };
        for _ in 0..back {
            self.output(b'\x08');
        }
    }
}

/// Whether `ECHOCTL` echoes `c` as `^X`.
fn caret(c: u8) -> bool {
    u8::is_ascii_control(&c) && c != b'\t'
}
