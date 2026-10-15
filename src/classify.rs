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
//!    only what was typed: what follows an edit on the line typed on, till
//!    a key submits it, may be the echo of the text typed on that line, and
//!    a text that is no part of it is output there too; what follows
//!    anything else, or shows elsewhere, is output. Text typed ahead of a
//!    program's read may be its echo when a line editor (the terminal raw)
//!    draws it again.
//!
//! Anything before the first input is output. What stays ambiguous is said
//! in `docs/agents.md`.
use crate::log::{InputSeq, Origin, Source, Unit};
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
    pub seq: InputSeq,
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
}

impl Classes {
    /// Notes that `bytes`, `input`, were written at `now`, the terminal's
    /// modes then `modes` (if they could be read).
    pub fn wrote(
        &mut self,
        input: Input,
        bytes: &[u8],
        modes: Option<&fuxix::terminal::Modes>,
        now: Instant,
    ) {
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
        // Text typed or pasted (its marks left out), not a key.
        let pasted =
            (bytes.strip_prefix(b"\x1b[200~")).map(|b| b.strip_suffix(b"\x1b[201~").unwrap_or(b));
        let typed = std::str::from_utf8(pasted.unwrap_or(bytes))
            .ok()
            .filter(|_| input.unit == Unit::Edit)
            .filter(|t| {
                !t.chars()
                    .any(|c| c.is_control() && !matches!(c, '\n' | '\r'))
            })
            .map(str::to_owned);
        if let Some(text) = typed.as_ref().filter(|_| input.ahead) {
            self.ahead_text.push(text.clone());
        }
        match (input.unit, &typed) {
            (Unit::Edit, Some(text)) => self.line.push_str(text),
            (Unit::Edit, None) => {}
            (Unit::Submit, _) => self.line.clear(),
        }
        self.written = Some(Written {
            input,
            kernel,
        });
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
        let (Some(written), false) = (self.written.as_ref(), self.pending.is_empty()) else {
            return vec![(piece.len(), rest)];
        };
        // The kernel's echo, found in order: a byte that is the next one
        // it echoes is its.
        let echo = Origin::Echo {
            of: written.input.seq,
        };
        let mut runs: Vec<(usize, Origin)> = Vec::new();
        for (at, byte) in piece.iter().enumerate() {
            let origin = match self.pending.front() == Some(byte) {
                true => {
                    self.pending.pop_front();
                    echo
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
    /// drawing the line it reads); or, after an edit on the line typed on,
    /// or as a line editor (`reads`) draws text typed ahead of its read,
    /// what was typed. An echo shows only what was typed: a text that is no
    /// part of it is output wherever it shows.
    pub fn echo(
        &self,
        origin: Origin,
        print: &fux_vt::Print<'_>,
        line: impl FnOnce() -> fux_vt::RowId,
        reads: bool,
    ) -> Echo {
        let ahead = || {
            let ahead = self.ahead_text.concat();
            let drawn = reads && !ahead.trim().is_empty() && !print.text.trim().is_empty();
            match drawn {
                true => Echo::Of(ahead.into()),
                false => Echo::Not,
            }
        };
        match origin {
            Origin::Output { by: Source::Screen } => Echo::Not,
            Origin::Output { .. } => ahead(),
            Origin::Echo { .. } => Echo::Is,
            Origin::Edited { line: typed, shell, .. } if typed == Some(line()) => match shell {
                true => Echo::Is,
                false => Echo::Of(self.line.as_str().into()),
            },
            Origin::Edited { .. } => Echo::Not,
        }
    }

    /// Where output that is not the kernel's echo came from.
    fn rest(&self, screen: &fux_vt::Screen, marked: bool) -> Origin {
        let full = screen.mode(fux_vt::Mode::AlternateScreen);
        let Some(written) = self.written.as_ref() else {
            return Origin::Output { by: Source::Order };
        };
        // A full-screen program's: its echo of an edit on the line typed
        // on (a field, a query), and the rest output.
        if full {
            return match written.input.unit {
                Unit::Edit => Origin::Edited {
                    of: written.input.seq,
                    line: written.input.line,
                    shell: false,
                },
                Unit::Submit => Origin::Output { by: Source::Screen },
            };
        }
        let commands = screen.commands();
        let reading = marked && commands.prompt && !commands.running;
        match (reading, written.input.unit) {
            // A prompt drawn since input typed ahead: the shell draws the
            // line typed ahead again, as it reads it.
            (true, Unit::Submit) => Origin::Output { by: Source::Marks },
            (false, _) if written.kernel => Origin::Output { by: Source::Kernel },
            (_, Unit::Edit) => Origin::Edited {
                of: written.input.seq,
                line: written.input.line,
                shell: reading,
            },
            (false, Unit::Submit) => Origin::Output { by: Source::Order },
        }
    }
}
