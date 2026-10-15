//! What the kernel echoes as fux types into a pane. In line mode the
//! kernel, not the program, echoes the keys and edits the line as they
//! come; [`Echo`] works out those bytes exactly, as the kernel writes them
//! back to the pty's master, so that the echo of typed keys can be told
//! apart from the program's own output.
//!
//! Linux's `n_tty` (`drivers/tty/n_tty.c`) and the BSD rules macOS keeps
//! (xnu's `bsd/kern/tty.c`) agree on what is modelled here, but for the few
//! rules a [`Discipline`] says; [`Kernel`] is the one this build runs on.
//!
//! The model covers text (a UTF-8 character erased whole under `IUTF8`),
//! carriage returns and newlines (`ICRNL`, `INLCR`, `IGNCR`, `ONLCR`,
//! `OCRNL`), `ECHO`, `ECHONL`, `ECHOCTL`, erase under `ECHOE`, kill, word
//! erase, end of line and file, and the signal characters. For the rest it
//! answers `None`, rather than guess: the caller then tells echo by order.
//! That is literal next, reprint, discard, status, flow control, erasing
//! with `ECHOPRT` or without `ECHOE`, erasing a tab, tabs written as
//! spaces, `ONOCR`, modes that rewrite input or output (`EXTPROC`, `IUCLC`,
//! `OLCUC`, `PARMRK`, `ALTWERASE`), and on macOS a kill and an erase of
//! what an earlier write typed. Two assumptions hold for both: a program
//! that turns line mode off and on again has read what was typed in
//! between, and fux reads the master's echo as it comes, so an interrupt's
//! flush finds none of it left over from earlier writes.
use fuxix::terminal::Modes;
use std::marker::PhantomData;

/// The line discipline of the kernel this build runs on.
#[cfg(any(target_os = "linux", target_os = "android"))]
pub type Kernel = Linux;
#[cfg(target_os = "macos")]
pub type Kernel = Macos;

/// Where one kernel's rules differ from the other's.
pub trait Discipline {
    /// Whether an erase of a character an earlier write typed may retype
    /// the line instead: macOS's does once the program wrote since
    /// (`t_rocount`), which fux cannot see in time.
    const RETYPES: bool;
    /// Whether the end-of-file character is echoed.
    const ECHOES_EOF: bool;
    /// Whether a suspend flushes the echo not yet read, as an interrupt
    /// does.
    const SUSPEND_FLUSHES_ECHO: bool;
    /// Whether `ECHOCTL` echoes a newline that does not end a line as `^J`.
    const CARET_NEWLINE: bool;
    /// Whether a kill, echoed, erases the line character by character
    /// rather than echoing the kill character; `None` when the model
    /// leaves it out.
    fn kill_erases(modes: &Modes) -> Option<bool>;
    /// Whether a word erase takes `c`, a character's first byte, as part
    /// of a word.
    fn word(c: u8) -> bool;
}

/// Linux's `n_tty`.
#[derive(Default, Debug)]
pub struct Linux;

impl Discipline for Linux {
    const RETYPES: bool = false;
    const ECHOES_EOF: bool = false;
    const SUSPEND_FLUSHES_ECHO: bool = true;
    const CARET_NEWLINE: bool = true;

    /// `eraser`, which also wants `ECHOE`.
    fn kill_erases(m: &Modes) -> Option<bool> {
        Some(m.echok && m.echoke)
    }

    /// The kernel's `isalnum`, or an underscore: ASCII's, and Latin-1's
    /// letters, which every UTF-8 lead byte from 0xc0 reads as but two.
    fn word(c: u8) -> bool {
        c.is_ascii_alphanumeric() || c == b'_' || (c >= 0xc0 && c != 0xd7 && c != 0xf7)
    }
}

/// macOS's BSD line discipline.
#[derive(Default, Debug)]
pub struct Macos;

impl Discipline for Macos {
    const RETYPES: bool = true;
    const ECHOES_EOF: bool = true;
    const SUSPEND_FLUSHES_ECHO: bool = false;
    const CARET_NEWLINE: bool = false;

    /// It erases only when every character in the line was echoed since
    /// the program last wrote, which fux cannot see in time.
    fn kill_erases(_: &Modes) -> Option<bool> {
        None
    }

    /// A word ends at a blank.
    fn word(c: u8) -> bool {
        c != b' ' && c != b'\t'
    }
}

/// The echo of what is typed into one terminal: the line the kernel holds,
/// so that an erase or a kill erases what the kernel would.
#[derive(Default, Debug)]
pub struct Echo<K = Kernel> {
    kernel: PhantomData<K>,
    /// What was typed since the last line end, in line mode.
    line: Vec<u8>,
    /// How much of `line` was typed before this write.
    start: usize,
    /// Whether line editing was on at the last write.
    canonical: Option<bool>,
    /// Once the kernel's state is unknown, it stays so.
    lost: bool,
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

/// An editing character of line mode.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Edit {
    Char,
    Word,
    Line,
}

impl<K: Discipline> Echo<K> {
    /// What the kernel echoes as `input` is written under `modes`, after
    /// output processing (`OPOST`, `ONLCR`...). `None` when some byte or
    /// mode is outside the model: the caller then falls back. Once the
    /// kernel's line is unknown, every later write is `None` too; a new
    /// `Echo` starts again from an empty line.
    pub fn echo(&mut self, modes: &Modes, input: &[u8]) -> Option<Vec<u8>> {
        // Turning line mode on or off commits what was typed.
        if self.canonical.replace(modes.canonical) != Some(modes.canonical) {
            self.line.clear();
        }
        let m = modes;
        self.lost |= m.extproc || m.iuclc || m.altwerase;
        if self.lost {
            return None;
        }
        self.start = self.line.len();
        let mut write = Write {
            modes,
            out: Vec::new(),
            known: !(m.opost && m.olcuc),
            caret_newline: K::CARET_NEWLINE,
        };
        for &c in input {
            if self.byte(&mut write, c).is_none() {
                self.lost = true;
                return None;
            }
        }
        (write.known && write.out.len() <= OUTPUT_QUEUE).then_some(write.out)
    }

    /// Takes one typed byte (`n_tty_receive_buf_standard`, `ttyinput`):
    /// `None` when the kernel's state is lost.
    fn byte(&mut self, w: &mut Write, typed: u8) -> Option<()> {
        let m = w.modes;
        let typed = if m.istrip { typed & 0x7f } else { typed };
        let left_out = |c| {
            (m.parmrk && c == 0xff)
                || (m.ixon && (is(m.start, c) || is(m.stop, c)))
                || (m.iexten && [m.lnext, m.reprint, m.discard, m.status].contains(&Some(c)))
        };
        if left_out(typed) {
            return None;
        }
        if m.isig && [m.intr, m.quit, m.susp].contains(&Some(typed)) {
            // Unless `NOFLSH`, the line is thrown away, and with it the
            // echo not yet read.
            if !m.noflsh {
                self.line.clear();
                if K::SUSPEND_FLUSHES_ECHO || is(m.intr, typed) || is(m.quit, typed) {
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
        if is(m.erase, c) {
            return self.edit(w, Edit::Char, c);
        }
        if is(m.kill, c) {
            // Linux takes it as a word erase when it is that too.
            if is(m.werase, c) {
                return None;
            }
            return self.edit(w, Edit::Line, c);
        }
        if m.iexten && is(m.werase, c) {
            return self.edit(w, Edit::Word, c);
        }
        if c == b'\n' || is(m.eof, c) || is(m.eol, c) || (m.iexten && is(m.eol2, c)) {
            self.line.clear();
        } else if self.line.len() < LONGEST_LINE {
            self.line.push(c);
        } else {
            return None;
        }
        if c == b'\n' {
            if m.echo || m.echonl {
                w.output(b'\n');
            }
        } else if !is(m.eof, c) {
            w.echo(c);
        } else if K::ECHOES_EOF {
            w.eof(c);
        }
        Some(())
    }

    /// An erase, word erase or kill (`eraser`, `ttyrub`), typed as `c`.
    fn edit(&mut self, w: &mut Write, kind: Edit, c: u8) -> Option<()> {
        let m = w.modes;
        if m.echo && m.echoprt {
            return None;
        }
        // Without `ECHOE` the erase character is echoed, on macOS once a
        // byte; macOS echoes a kill's newline under `ECHONL` alone.
        w.known &= !(m.echo && !m.echoe) && !(kind == Edit::Line && !m.echo && m.echonl);
        if kind == Edit::Line && m.echo {
            match K::kill_erases(m) {
                None => w.known = false,
                Some(true) => {}
                Some(false) => {
                    if !self.line.is_empty() {
                        w.echo(c);
                        if m.echok {
                            w.output(b'\n');
                        }
                    }
                    self.line.clear();
                    return Some(());
                }
            }
        }
        let mut seen_word = false;
        // A character's continuation bytes go with it; only continuation
        // bytes left are never erased.
        while let Some(at) = (self.line.iter()).rposition(|&b| !(m.iutf8 && continuation(b))) {
            let first = *self.line.get(at)?;
            if kind == Edit::Word {
                if K::word(first) {
                    seen_word = true;
                } else if seen_word {
                    break;
                }
            }
            if K::RETYPES && m.echo && at < self.start {
                w.known = false;
            }
            self.line.truncate(at);
            self.start = self.start.min(at);
            if m.echo {
                w.rub(first);
            }
            if kind == Edit::Char {
                break;
            }
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
    /// [`Discipline::CARET_NEWLINE`].
    caret_newline: bool,
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
        } else if m.echo && m.echoctl && caret(c) && (c != b'\n' || self.caret_newline) {
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

    /// Erases `c`, a character's first byte, from the screen: a control
    /// character echoed as `^X` takes two columns, one echoed as itself
    /// none.
    fn rub(&mut self, c: u8) {
        let columns = match c {
            b'\t' => {
                self.known = false;
                0
            }
            _ if !cntrl(c) => 1,
            _ if self.modes.echoctl => 2,
            _ => 0,
        };
        for _ in 0..columns {
            for c in *b"\x08 \x08" {
                self.output(c);
            }
        }
    }
}

/// Whether `c` is the character `char`, which may be disabled.
fn is(char: Option<u8>, c: u8) -> bool {
    char == Some(c)
}

/// Whether `c` continues a UTF-8 character.
fn continuation(c: u8) -> bool {
    c & 0xc0 == 0x80
}

/// The kernels' `iscntrl`: ASCII's control characters only.
fn cntrl(c: u8) -> bool {
    c < 0x20 || c == 0x7f
}

/// Whether `ECHOCTL` echoes `c` as `^X`.
fn caret(c: u8) -> bool {
    cntrl(c) && c != b'\t'
}
