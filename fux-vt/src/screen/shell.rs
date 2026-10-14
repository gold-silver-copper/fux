//! Shell integration's command marks (OSC 133, FinalTerm's semantic prompts):
//! `C` where a command's output starts, `D ; status` where it ends and how.
//! `C ; late`, sent only as the command ends (a line the shell could not
//! parse, so never ran, has no start mark of its own), starts its output
//! on the row after the line of the prompt it was typed at. A host reads them to know when a command typed into a shell is done, its
//! status, and its output ([`Screen::commands`], [`Screen::last_output`]).

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
    /// A command's output began (`C`) and has not ended (`D`).
    pub running: bool,
    /// How many commands have ended (`D`): a host waits for it to grow.
    pub finished: u64,
    /// The status the last command ended with, if the shell said it.
    pub last_status: Option<i32>,
    /// Where the last command's output began and ended.
    pub last_output: Option<(Mark, Mark)>,
    /// Where the running command's output began.
    pub output_start: Option<Mark>,
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
                self.commands.running = true;
                self.commands.output_start = Some(match late {
                    true => self.after_prompt().unwrap_or(here),
                    false => here,
                });
            }
            b"D" => {
                let status = option
                    .and_then(|s| std::str::from_utf8(s).ok())
                    .and_then(|s| s.parse().ok());
                // A D with no C before it ends no command: a shell sends one
                // with every prompt, the first included.
                if let Some(start) = self.commands.output_start.take() {
                    self.commands.last_output = Some((start, here));
                    self.commands.last_status = status;
                    self.commands.finished = self.commands.finished.wrapping_add(1);
                }
                self.commands.running = false;
            }
            _ => {}
        }
    }

    /// The main screen erased whole while a command runs (`clear`): its
    /// output begins again at the screen's top, what was above gone. The
    /// alternate screen erased (a full-screen program the command ran) is
    /// none of it: the marks are the main screen's.
    pub(crate) fn screen_erased(&mut self) {
        if !self.commands.running || self.mode(crate::Mode::AlternateScreen) {
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

    /// The start of the row after the line the last prompt above the
    /// cursor starts (its soft-wrapped rows with it), if that is not below
    /// the cursor: where what a line typed at it brought begins.
    fn after_prompt(&self) -> Option<Mark> {
        let grid = self.grid();
        let cursor = grid
            .history_len()
            .checked_add(usize::from(grid.cursor.row()))?;
        let prompt = (0..=cursor)
            .rev()
            .find(|i| grid.row_at(*i).is_some_and(|r| r.starts_prompt()))?;
        let next = (prompt..=cursor)
            .find(|i| grid.row_at(*i).is_some_and(|r| !r.wrapped()))?
            .checked_add(1)?;
        let row = grid.row_at(next).filter(|_| next <= cursor)?;
        Some(Mark {
            row: row.id(),
            col: 0,
        })
    }

    fn cursor_mark(&self) -> Option<Mark> {
        let grid = self.grid();
        let index = grid
            .history_len()
            .checked_add(usize::from(grid.cursor.row()))?;
        let row = grid.row_at(index)?.id();
        Some(Mark {
            row,
            col: grid.cursor.col(),
        })
    }
}
