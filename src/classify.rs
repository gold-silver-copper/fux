//! Whether output is the echo of what was typed or the program's own,
//! decided as it arrives from what fux recorded, never from how it reads:
//!
//! 1. **The kernel's echo.** In line mode the terminal echoes input by
//!    fixed rules (`crate::echo`): those bytes, found in order in what
//!    follows the input, are echo, certainly, and the rest is output.
//! 2. **The shell's marks.** A shell fux marks (its marks carry the pane's
//!    nonce) says when it reads its command line: what it draws after a key
//!    that edits the line is the line's echo; what follows a line submitted
//!    is output; and keys written while it was not reading (a command ran,
//!    or no prompt was drawn yet) are typed ahead, which it draws again at
//!    its next prompt.
//! 3. **Order and place.** Input is written a piece at a time, each once
//!    the program took the one before (`crate::pane::Pace`). An echo shows
//!    only what was typed: what follows an edit, till a key submits the
//!    line, may be the echo of the text typed on it, wherever it lands, and
//!    a text that is no part of it is output; what follows anything else
//!    is output. Text typed ahead of a program's read may be its echo when
//!    a line editor (the terminal raw) draws it again.
//!
//! A full-screen program's screen is all output, as is anything before the
//! first input. What stays ambiguous is said in `docs/agents.md`.
use crate::log::{Origin, Unit};
use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// How long the kernel's echo of a piece of input is looked for in what
/// follows it: it comes within a millisecond or two.
const ECHO_WITHIN: Duration = Duration::from_secs(1);

/// What a run of printed text may be the echo of.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Echo {
    /// Nothing: it is output.
    Not,
    /// All of it: it is an echo.
    Is,
    /// This text typed, if anything: a text no part of it is output.
    Of(std::sync::Arc<str>),
}

/// A piece of input as it was written.
#[derive(Clone, Copy, Debug)]
pub struct Input {
    pub unit: Unit,
    /// The first row of the line the cursor was on as it went in.
    pub line: Option<fux_vt::RowId>,
    /// The input was typed ahead of the program's read: written while a
    /// shell fux marks runs a command or has drawn no prompt yet, or, in
    /// another program, while it was not reading.
    pub ahead: bool,
}

/// The last input written, as the output after it is told.
#[derive(Clone, Debug)]
struct Written {
    input: Input,
    /// The terminal edited or echoed input then (`ICANON` or `ECHO`), and
    /// the kernel's echo of it is known.
    kernel: bool,
}

/// What `classify` keeps between pieces.
#[derive(Default)]
pub struct Classes {
    written: Option<Written>,
    /// The kernel's echo of what was written, not yet seen, and until when
    /// it is looked for.
    pending: VecDeque<u8>,
    pending_until: Option<Instant>,
    kernel: crate::echo::Echo,
    /// The text typed ahead of the program's read (`Input::ahead`), none
    /// typed since while it read: a program that reads it later draws it
    /// again, its echo.
    ahead_text: Vec<String>,
    /// The text typed on the line since a key last submitted one: all an
    /// echo of an edit can show.
    line: String,
    /// The text the last echo was of, shared by every run it may be the
    /// echo of: one copy, however many runs a line editor draws.
    shared: Option<std::sync::Arc<str>>,
}

impl Classes {
    /// Notes that `bytes`, `input`, were written at `now`, the terminal's
    /// modes then `modes` (if they could be read): whether the kernel's echo
    /// of them is known, and so what follows them, exactly.
    pub fn wrote(
        &mut self,
        input: Input,
        bytes: &[u8],
        modes: Option<&fuxix::terminal::Modes>,
        now: Instant,
    ) -> bool {
        if !input.ahead {
            self.ahead_text.clear();
        }
        let echo = modes.and_then(|m| Some((m, self.kernel.echo(m, bytes)?)));
        let kernel = match echo {
            Some((modes, echo)) => {
                self.pending.extend(echo);
                self.pending_until = Some(crate::after(now, ECHO_WITHIN));
                modes.canonical || modes.echo
            }
            None => false,
        };
        // Text typed or pasted (its marks left out), not a key: what it
        // shows, its control characters (BSpace, C-u, Tab) aside.
        let pasted =
            (bytes.strip_prefix(b"\x1b[200~")).map(|b| b.strip_suffix(b"\x1b[201~").unwrap_or(b));
        let typed = std::str::from_utf8(pasted.unwrap_or(bytes))
            .ok()
            .filter(|t| input.unit == Unit::Edit && !t.starts_with('\x1b'))
            .map(|t| t.chars().filter(|c| !c.is_control()).collect::<String>());
        if let Some(text) = typed.as_ref().filter(|_| input.ahead) {
            self.ahead_text.push(text.clone());
        }
        match (input.unit, &typed) {
            (Unit::Edit, Some(text)) => self.line.push_str(text),
            (Unit::Edit, None) => {}
            (Unit::Submit, _) => self.line.clear(),
        }
        self.written = Some(Written { input, kernel });
        kernel
    }

    /// The text typed on the line since a key last submitted one.
    pub fn typed(&self) -> &str {
        &self.line
    }

    /// Whether text typed ahead waits to be drawn again: then `echoes`
    /// asks whether a line editor reads it now.
    pub fn ahead(&self) -> bool {
        !self.ahead_text.is_empty()
    }

    /// Where the bytes of `piece` came from, arriving at `now` on `screen`
    /// as it stands before them, `marked` if the pane's shell marks its
    /// commands with the pane's nonce: each run's end, and its origin.
    pub fn classify(
        &mut self,
        screen: &fux_vt::Screen,
        marked: bool,
        piece: &[u8],
        now: Instant,
    ) -> Vec<(usize, Origin)> {
        if self.pending_until.is_some_and(|until| now >= until) {
            self.pending.clear();
            self.pending_until = None;
        }
        let rest = self.rest(screen, marked);
        if self.written.is_none() || self.pending.is_empty() {
            return vec![(piece.len(), rest)];
        }
        // The kernel's echo, found in order: a byte that is the next one
        // it echoes is its.
        let mut runs: Vec<(usize, Origin)> = Vec::new();
        for (at, byte) in piece.iter().enumerate() {
            let origin = match self.pending.front() == Some(byte) {
                true => {
                    self.pending.pop_front();
                    Origin::Echo
                }
                false => rest,
            };
            match runs.last_mut() {
                Some((end, last)) if *last == origin => *end = at.saturating_add(1),
                _ => runs.push((at.saturating_add(1), origin)),
            }
        }
        runs
    }

    /// What `print`, which came from `origin` and went on the line that
    /// starts at `line()` (found only if asked), may be the echo of: none
    /// (output); everything (the kernel's echo, or a shell fux marks
    /// drawing the line it reads); or, after an edit till a key submits the
    /// line, or as a line editor (`reads`) draws text typed ahead of its
    /// read, what was typed. An echo shows only what was typed: a text
    /// that is no part of it is output wherever it shows.
    pub fn echo(
        &mut self,
        origin: Origin,
        print: &fux_vt::Print<'_>,
        line: impl FnOnce() -> fux_vt::RowId,
        reads: bool,
    ) -> Echo {
        let ahead = |classes: &mut Classes| {
            if classes.ahead_text.is_empty() {
                return Echo::Not;
            }
            let ahead = classes.ahead_text.concat();
            let drawn = reads && !ahead.trim().is_empty() && !print.text.trim().is_empty();
            match drawn {
                true => Echo::Of(classes.share(ahead)),
                false => Echo::Not,
            }
        };
        match origin {
            Origin::Screen => Echo::Not,
            Origin::Output => ahead(self),
            Origin::Echo => Echo::Is,
            // A shell fux marks draws the line it reads: all of it echo.
            Origin::Edited {
                line: typed,
                shell: true,
            } if typed == Some(line()) => Echo::Is,
            // Wherever it lands (another line printed above the line typed
            // moves it down), an echo shows only what was typed.
            Origin::Edited { .. } => {
                let line = self.line.clone();
                Echo::Of(self.share(line))
            }
        }
    }

    /// `text`, as the last echo shared it if it is the same.
    fn share(&mut self, text: String) -> std::sync::Arc<str> {
        match &self.shared {
            Some(shared) if **shared == *text => shared.clone(),
            Some(_) | None => self.shared.insert(text.into()).clone(),
        }
    }

    /// Where output that is not the kernel's echo came from.
    fn rest(&self, screen: &fux_vt::Screen, marked: bool) -> Origin {
        // A full-screen program's screen is all output: a field showing
        // what was typed is a text an agent may wait for, to know it went in.
        if screen.mode(fux_vt::Mode::AlternateScreen) {
            return Origin::Screen;
        }
        let Some(written) = self.written.as_ref() else {
            return Origin::Output;
        };
        let commands = screen.commands();
        let reading = marked && commands.prompt && !commands.running;
        match (reading, written.input.unit) {
            // The kernel echoes the keys and nothing else: the rest is output.
            (false, _) if written.kernel => Origin::Output,
            (_, Unit::Edit) => Origin::Edited {
                line: written.input.line,
                shell: reading,
            },
            // What followed a line submitted, or, at a prompt drawn since
            // input typed ahead, the shell drawing that line again as it
            // reads it.
            (_, Unit::Submit) => Origin::Output,
        }
    }
}
