//! Shell integration's command marks (OSC 133, FinalTerm's semantic prompts):
//! `C` where a command's output starts, `D ; status` where it ends and how.
//! `C ; late`, sent only as the command ends (a line the shell could not
//! parse, so never ran, has no start mark of its own), starts its output
//! on the row after the line last submitted, as the host said
//! (`Parser::note_submit`), else after the line of the prompt it was
//! typed at. A host reads them to know when a command typed into a shell is done, its
//! status, and its output ([`Screen::commands`], [`Commands::last_output`]).
//! Given nonces (`Parser::require_mark_nonce`), a mark counts only with the
//! shell's, or with the nested one while the shell's command runs: then the
//! program that command started (a remote shell's marks, relayed) says what
//! runs within it, until the shell marks again.

use super::Screen;
use crate::RowId;

/// A place in the retained rows: a row, which scrolling carries, and a
/// column.
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
}

impl Commands {
    /// Whether a command's output began (`C`) and has not ended (`D`).
    pub fn running(&self) -> bool {
        self.output_start.is_some()
    }
}

impl Screen {
    /// What the shell's marks say of its commands, on the primary screen,
    /// where a shell runs.
    pub fn commands(&self) -> Commands {
        self.commands
    }

    /// `C [; late]`, the output of the command just typed begins (`late`:
    /// began after its prompt's line), or `D [; status]`, it ended: called
    /// with the mark's payload after `133;`.
    pub(crate) fn command_mark(&mut self, command: &[u8], option: Option<&[u8]>) {
        let Some(here) = self.cursor_mark() else {
            return;
        };
        match command {
            b"C" => {
                let late = option == Some(&b"late"[..]);
                self.commands.prompt = false;
                let submitted = (self.submitted.take())
                    .and_then(|s| self.line_after(self.grid().index_of(s.row)?));
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
    /// output begins again at the screen's top, what was above gone. The
    /// alternate screen erased (a full-screen program the command ran) is
    /// none of it: the marks are the main screen's.
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

    /// A prompt was drawn on the cursor's row, which starts one now: the
    /// shell reads its command line, and a line submitted before is no
    /// longer its.
    pub(crate) fn prompted(&mut self) {
        let row = self.grid().cursor.row();
        self.grid_mut().set_prompt(row, true);
        self.commands.prompt = true;
        self.submitted = None;
    }

    /// The start of the row after the line the last prompt above the
    /// cursor starts (its soft-wrapped rows with it), if that is not below
    /// the cursor: where what a line typed at it brought begins.
    fn after_prompt(&self) -> Option<Mark> {
        let grid = self.grid();
        let prompt = (0..=grid.cursor_index()?)
            .rev()
            .find(|i| grid.row_at(*i).is_some_and(|r| r.starts_prompt()))?;
        self.line_after(prompt)
    }

    /// The start of the row after the line that row `at` is on, its
    /// soft-wrapped rows with it, if that is not below the cursor: where
    /// what followed a line submitted there, or typed at a prompt there,
    /// begins.
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
