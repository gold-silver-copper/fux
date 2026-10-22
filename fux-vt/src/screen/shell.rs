//! Shell integration's command marks (OSC 133, FinalTerm's semantic prompts):
//! `C` where a command's output starts, `D ; status` where it ends and how.
//! `C ; late`, sent only as the command ends (a line the shell could not
//! parse has no start mark of its own), starts its output on the row after
//! the line last submitted (`Parser::note_submit`), else after its prompt's
//! line. A host reads them to know when a command is done, its status and
//! its output ([`Screen::commands`]).
//! Given nonces (`Parser::require_mark_nonce`), a mark counts only with the
//! shell's, or with the nested one while the shell's command runs: then the
//! program that command started (a remote shell's marks, relayed) says what
//! runs within it, until the shell marks again.
//!
//! `S ; activity=STATE`, a state report, says what the program a relay runs
//! does (a remote one, which the host cannot probe). It counts as a mark
//! does, and holds until the next mark ([`Commands::report`]).

use super::Screen;
use crate::RowId;

/// A place in the retained rows, which scrolling carries.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Mark {
    /// The row.
    pub row: RowId,
    /// The column, from 0.
    pub col: u16,
}

/// What the shell's marks say of its commands.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Commands {
    /// How many commands have begun (`C` with none running; a `C` while one
    /// runs, a relay placing its start anew, begins none).
    pub started: u64,
    /// How many commands have ended (`D`): a host waits for it to grow.
    pub finished: u64,
    /// The status the last command ended with, if the shell said it.
    pub last_status: Option<i32>,
    /// Where the last command's output began and ended.
    pub last_output: Option<(Mark, Mark)>,
    /// Where the running command's output began (`C`); `None` once it
    /// ended (`D`), and before any began.
    pub output_start: Option<Mark>,
    /// The running command's output began on the alternate screen: the
    /// full-screen program there marks commands of its own (a remote
    /// shell's), rather than being a command the shell ran.
    pub full_screen: bool,
    /// The shell drew a prompt (`A`) and has started no command since
    /// (`C`): it is reading its command line.
    pub prompt: bool,
    /// How many marks have counted, reports among them.
    pub marks: u64,
    /// The last state report that counted (`S`), if any.
    pub report: Option<Report>,
}

/// What a relay said the program it runs does (`OSC 133 ; S ;
/// activity=STATE`): the states of a terminal's foreground group, as a host
/// probes them for a program of its own.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reported {
    /// Blocked reading its terminal, with no time limit.
    Reading,
    /// Waiting on its terminal with a time limit: it may draw again alone.
    Polling,
    /// Blocked on something else.
    Waiting,
    /// Running, sleeping on a timer, or starting.
    Busy,
    /// Every thread waits, on what the system does not name (macOS).
    Idle,
    /// The system does not say.
    Unknown,
    /// It ended, having read every key the relay sent it: said once, as the
    /// relay goes, so a host knows the last keys reached it.
    Ended,
}

impl Reported {
    /// The state a report's `activity=` names; `None` for one unknown.
    pub fn named(name: &[u8]) -> Option<Reported> {
        Some(match name {
            b"reading" => Reported::Reading,
            b"polling" => Reported::Polling,
            b"waiting" => Reported::Waiting,
            b"busy" => Reported::Busy,
            b"idle" => Reported::Idle,
            b"unknown" => Reported::Unknown,
            b"ended" => Reported::Ended,
            _ => return None,
        })
    }
}

/// A state report that counted, and where it came among the marks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Report {
    /// The state it said.
    pub activity: Reported,
    /// [`Commands::marks`] as it came.
    pub mark: u64,
}

impl Commands {
    /// Whether a command's output began (`C`) and has not ended (`D`).
    pub fn running(&self) -> bool {
        self.output_start.is_some()
    }

    /// The state the last report said, if no mark came after it.
    pub fn reported(&self) -> Option<Reported> {
        (self.report)
            .filter(|r| r.mark == self.marks)
            .map(|r| r.activity)
    }
}

impl Screen {
    /// What the shell's marks say of its commands.
    pub fn commands(&self) -> Commands {
        self.commands
    }

    /// `C [; late]`, a command's output begins, or `D [; status]`, it ended.
    pub(crate) fn command_mark(&mut self, command: &[u8], option: Option<&[u8]>) {
        let Some(here) = self.cursor_mark() else {
            return;
        };
        match command {
            b"C" => {
                let late = option == Some(&b"late"[..]);
                if !self.commands.running() {
                    self.commands.started = self.commands.started.wrapping_add(1);
                }
                self.commands.prompt = false;
                // Looked for on the screen alone (THREATS.md B29): a line
                // submitted, or a prompt, scrolled into history by the time
                // its output starts marks none, and the output starts here.
                let submitted = (self.submitted.take())
                    .and_then(|s| self.line_after(self.grid().screen_index_of(s.row)?));
                self.commands.output_start = Some(match late {
                    true => submitted.or_else(|| self.after_prompt()).unwrap_or(here),
                    false => here,
                });
                self.commands.full_screen = self.mode(crate::Mode::AlternateScreen);
            }
            b"D" => {
                let status = option
                    .and_then(|s| str::from_utf8(s).ok())
                    .and_then(|s| s.parse().ok());
                // A D with no C before it ends no command: a shell sends one
                // with every prompt, the first included.
                if let Some(start) = self.commands.output_start.take() {
                    self.commands.last_output = Some((start, here));
                    self.commands.last_status = status;
                    self.commands.finished = self.commands.finished.wrapping_add(1);
                }
            }
            _ => {}
        }
    }

    /// `S ; activity=STATE`: one naming no state it knows does not count.
    pub(crate) fn report(&mut self, options: &[u8]) {
        let activity = (options.split(|b| *b == b';'))
            .find_map(|o| o.strip_prefix(b"activity="))
            .and_then(Reported::named);
        if let Some(activity) = activity {
            self.commands.marks = self.commands.marks.wrapping_add(1);
            let mark = self.commands.marks;
            self.commands.report = Some(Report { activity, mark });
        }
    }

    /// Whether a mark of a program the shell runs (the nested nonce's)
    /// counts: while a command the shell started runs. The first sets that
    /// command aside: the program's marks say what runs within it.
    pub(crate) fn nest(&mut self) -> bool {
        if self.outer.is_none() {
            let Some(start) = self.commands.output_start.take() else {
                return false;
            };
            self.outer = Some((start, self.commands.full_screen));
        }
        true
    }

    /// The shell marks again: its command set aside runs once more, to end
    /// as its marks say, whatever the program it ran left running.
    pub(crate) fn unnest(&mut self) {
        if let Some((start, full_screen)) = self.outer.take() {
            self.commands.output_start = Some(start);
            self.commands.full_screen = full_screen;
        }
    }

    /// The main screen erased whole while a command runs (`clear`): its
    /// output begins again at the screen's top. The alternate screen's
    /// erases are ignored: the marks are the main screen's.
    pub(crate) fn screen_erased(&mut self) {
        if !self.commands.running() || self.mode(crate::Mode::AlternateScreen) {
            return;
        }
        let grid = self.grid();
        let top = grid.row_at(grid.history_len()).map(|row| Mark {
            row: row.id(),
            col: 0,
        });
        if let Some(top) = top {
            self.commands.output_start = Some(top);
        }
    }

    /// A prompt was drawn on the cursor's row: a line submitted before is
    /// no longer its.
    pub(crate) fn prompted(&mut self) {
        let (row, version) = (self.grid().cursor.row(), self.version);
        self.grid_mut().set_prompt(row, true, version);
        self.commands.prompt = true;
        self.submitted = None;
    }

    /// [`Self::line_after`] the last prompt above the cursor on the screen.
    fn after_prompt(&self) -> Option<Mark> {
        let grid = self.grid();
        let prompt = (grid.history_len()..=grid.cursor_index()?)
            .rev()
            .find(|i| grid.row_at(*i).is_some_and(|r| r.starts_prompt()))?;
        self.line_after(prompt)
    }

    /// The start of the row after the line (soft-wrapped rows included)
    /// that row `at` is on, if that is not below the cursor.
    fn line_after(&self, at: usize) -> Option<Mark> {
        let grid = self.grid();
        let cursor = grid.cursor_index()?;
        let next = (at..=cursor)
            .find(|i| grid.row_at(*i).is_some_and(|r| !r.wrapped()))?
            .checked_add(1)?;
        let row = grid.row_at(next).filter(|_| next <= cursor)?;
        Some(Mark {
            row: row.id(),
            col: 0,
        })
    }

    pub(crate) fn cursor_mark(&self) -> Option<Mark> {
        let grid = self.grid();
        let row = grid.row_at(grid.cursor_index()?)?.id();
        Some(Mark {
            row,
            col: grid.cursor.col(),
        })
    }
}
