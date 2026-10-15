//! Whether a piece of output is the echo of what was typed or the
//! program's own output, decided as the piece arrives, from three sources
//! in a fixed order of trust:
//!
//! 1. **The shell's marks.** While a shell fux marked (its marks carry the
//!    pane's nonce) reads its command line, what it draws is the echo of
//!    the line typed, and what follows a line submitted is output.
//! 2. **The kernel's echo.** In line mode the terminal echoes input by
//!    fixed rules (`crate::echo`): those bytes, found in order in what
//!    follows the input, are echo, and the rest is output.
//! 3. **Order.** Input is written a piece at a time, each once the program
//!    has taken the one before (`crate::pane::Pace`): what follows an edit
//!    at once ([`EDIT_WINDOW`]) is the echo on the line typed on, and output
//!    elsewhere; what follows anything else, or comes later, is output.
//!
//! A full-screen program's screen is all output, as is anything before
//! the first input.
use crate::log::{Certain, InputSeq, Origin, Source, Unit};
use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// How long the kernel's echo of a piece of input is looked for in what
/// follows it: it comes within a millisecond or two.
const ECHO_WITHIN: Duration = Duration::from_secs(1);
/// How long after an edit what a program draws on the line typed on is
/// taken as its echo: a line editor draws it as it handles the key, within
/// milliseconds; a program that answers the keys later (after a pause, a
/// sleep, some work) answers with output.
pub const EDIT_WINDOW: Duration = Duration::from_millis(250);

/// A piece of input as it was written.
#[derive(Clone, Copy, Debug)]
pub struct Input {
    pub seq: InputSeq,
    pub unit: Unit,
    /// The first row of the line the cursor was on as it went in.
    pub line: Option<fux_vt::RowId>,
    /// The program was reading its terminal: else the input was typed
    /// ahead of its read.
    pub reading: bool,
}

/// The last input written, as the output after it is told.
#[derive(Clone, Debug)]
struct Written {
    input: Input,
    at: Instant,
    /// The text it typed, if it typed text (not a key).
    typed: Option<String>,
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
    /// Input went in while the program was not reading (typed ahead),
    /// and none since while it was; and the text typed so. A program that
    /// reads it later draws it again: its echo.
    ahead: bool,
    ahead_text: Vec<String>,
    /// The shell drew a prompt since the last input.
    prompted: bool,
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
        self.ahead = !input.reading;
        if input.reading {
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
        let typed = std::str::from_utf8(bytes)
            .ok()
            .filter(|t| input.unit == Unit::Edit && !t.chars().any(char::is_control))
            .map(str::to_owned);
        if let Some(text) = typed.as_ref().filter(|_| self.ahead) {
            self.ahead_text.push(text.clone());
        }
        self.written = Some(Written {
            input,
            at: now,
            typed,
            kernel,
        });
        self.prompted = false;
    }

    /// Whether text typed ahead waits to be drawn again: then `echoes`
    /// asks whether the terminal is raw.
    pub fn ahead(&self) -> bool {
        !self.ahead_text.is_empty()
    }

    /// The shell drew a prompt.
    pub fn prompted(&mut self) {
        self.prompted = self.written.is_some();
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
        let rest = self.rest(screen, marked, now);
        let (Some(written), false) = (self.written.as_ref(), self.pending.is_empty()) else {
            return vec![(piece.len(), rest)];
        };
        // The kernel's echo, found in order: a byte that is the next one
        // it echoes is its.
        let echo = Origin::Echo {
            of: written.input.seq,
            by: Certain::Kernel,
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

    /// Whether `print`, which came from `origin` and went on the line that
    /// starts at `line()` (found only if asked), is an echo: one of the kernel's or the shell's;
    /// after an edit, on the line typed on, a redraw after a key, or text
    /// that holds what was typed or is part of it, as a line editor draws it
    /// where it was typed (not a reply the program writes there); or text
    /// typed ahead of the program's read, drawn again as it reads it, with
    /// the terminal `raw` (no line editing: a line editor's).
    pub fn echoes(
        &self,
        origin: Origin,
        print: &fux_vt::Print,
        line: impl FnOnce() -> fux_vt::RowId,
        raw: bool,
    ) -> bool {
        // Text typed ahead, drawn again as a line editor (the terminal
        // `raw` now, as it reads keys) reads it.
        let again = || {
            raw && (self.ahead_text.iter())
                .any(|t| !t.trim().is_empty() && print.text.contains(t.as_str()))
        };
        match origin {
            Origin::Output { by: Source::Screen } => false,
            Origin::Output { .. } => again(),
            Origin::Echo { .. } => true,
            Origin::Edited { line: typed, .. } => {
                let text = self.written.as_ref().and_then(|w| w.typed.as_deref());
                typed == Some(line())
                    && text.is_none_or(|t| print.text.contains(t) || t.contains(&print.text))
            }
        }
    }

    /// Where output that is not the kernel's echo came from.
    fn rest(&self, screen: &fux_vt::Screen, marked: bool, now: Instant) -> Origin {
        if screen.mode(fux_vt::Mode::AlternateScreen) {
            return Origin::Output { by: Source::Screen };
        }
        let Some(written) = self.written.as_ref() else {
            return Origin::Output { by: Source::Order };
        };
        let commands = screen.commands();
        let reading = marked && commands.prompt && !commands.running;
        match (reading, written.input.unit) {
            // A prompt drawn since input typed ahead: the shell draws the
            // line typed ahead again, as it reads it.
            (true, _) if self.prompted && self.ahead => Origin::Echo {
                of: written.input.seq,
                by: Certain::Marks,
            },
            (true, Unit::Submit) => Origin::Output { by: Source::Marks },
            (false, _) if written.kernel => Origin::Output { by: Source::Kernel },
            (_, Unit::Edit) if now < crate::after(written.at, EDIT_WINDOW) => Origin::Edited {
                of: written.input.seq,
                line: written.input.line,
            },
            (true, Unit::Edit) => Origin::Output { by: Source::Marks },
            (false, Unit::Edit | Unit::Submit) => Origin::Output { by: Source::Order },
        }
    }
}
