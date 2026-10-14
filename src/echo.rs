//! What the kernel echoes as fux types into a pane. In line mode the
//! kernel, not the program, echoes the keys and edits the line as they
//! come; [`Echo`] works out those bytes exactly, as the kernel writes them
//! back to the pty's master, so that the echo of typed keys can be told
//! apart from the program's own output.
//!
//! Each kernel has its own rules, a [`Discipline`]: Linux's `n_tty`
//! (`drivers/tty/n_tty.c`) and the BSD rules macOS keeps (xnu's
//! `bsd/kern/tty.c`). [`Kernel`] is the one this build runs on; both
//! compile everywhere, so either can be checked on any machine.
//!
//! What the model leaves out it answers `None` for, rather than guess:
//! erasing a tab, and tabs written as spaces, whose columns depend on the
//! program's output too; a carriage return under `ONOCR`, for the same
//! reason; literal next, prompt-style erase (`ECHOPRT`), the stop
//! character, and modes that rewrite input or output (`EXTPROC`, `IUCLC`,
//! `OLCUC`); on macOS also reprint, status, discard, and echo past the
//! output queue's 1024 bytes. Two assumptions hold for both: a program
//! that turns line mode off and on again has read what was typed in
//! between, and fux reads the master's echo as it comes, so an interrupt's
//! flush finds none of it left over from earlier writes.
use fuxix::terminal::Modes;

mod n_tty;
mod xnu;

pub use n_tty::NTty;
pub use xnu::Xnu;

/// The line discipline of the kernel this build runs on.
#[cfg(any(target_os = "linux", target_os = "android"))]
pub type Kernel = NTty;
#[cfg(target_os = "macos")]
pub type Kernel = Xnu;

/// The kernel's state is no longer known: what it holds, or how it will
/// take the next key, depends on something the model leaves out.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Lost;

/// One kernel's rules for echoing and editing typed input.
pub trait Discipline: Default {
    /// Takes one write of typed input under `modes`, as the kernel takes a
    /// write to the master: what it echoes, or `None` when that is outside
    /// the model though the line is still known.
    fn input(&mut self, modes: &Modes, input: &[u8]) -> Result<Option<Vec<u8>>, Lost>;

    /// Line editing was turned on or off: the line typed so far is no
    /// longer one to edit.
    fn mode_changed(&mut self);

    /// The program on the terminal wrote output.
    fn program_wrote(&mut self);
}

/// The echo of what is typed into one terminal: the line the kernel holds,
/// so that an erase or a kill erases what the kernel would.
#[derive(Default, Debug)]
pub struct Echo<K = Kernel> {
    kernel: K,
    /// Whether line editing was on at the last write.
    canonical: Option<bool>,
    /// Once the kernel's state is unknown, it stays so.
    lost: bool,
}

impl<K: Discipline> Echo<K> {
    /// What the kernel echoes as `input` is written under `modes`, after
    /// output processing (`OPOST`, `ONLCR`...). `None` when some byte or
    /// mode is outside the model: the caller then falls back. Once the
    /// kernel's line is unknown, every later write is `None` too; a new
    /// `Echo` starts again from an empty line.
    pub fn echo(&mut self, modes: &Modes, input: &[u8]) -> Option<Vec<u8>> {
        if self.canonical.is_some_and(|was| was != modes.canonical) {
            self.kernel.mode_changed();
        }
        self.canonical = Some(modes.canonical);
        if self.lost {
            return None;
        }
        self.kernel.input(modes, input).unwrap_or_else(|Lost| {
            self.lost = true;
            None
        })
    }

    /// The program wrote output: on macOS, the next erase in the line then
    /// retypes it.
    pub fn program_wrote(&mut self) {
        self.kernel.program_wrote();
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

/// The letter a control character echoes as after a `^`: `^C`, `^?`.
fn caret(c: u8) -> u8 {
    c ^ 0x40
}
